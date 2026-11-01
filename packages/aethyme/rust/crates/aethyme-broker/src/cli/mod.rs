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
use crate::cli_output::out;

mod event_watch;
mod flags;
mod gates;
mod gc_storage;
mod git_gh;
mod insights;
mod leases;
mod merge_chain;
mod render;
mod report;
mod resources;
mod review;
mod session;
mod ship;
mod status;
mod status_compact;
mod submit;
mod surface;
mod telemetry;
#[cfg(test)]
mod tests;
mod watch;

use flags::*;
use gates::*;
use gc_storage::*;
use git_gh::*;
use insights::*;
use leases::*;
use merge_chain::*;
use render::*;
use report::*;
use resources::*;
use review::*;
use session::*;
use ship::*;
use status::*;
use submit::*;
use telemetry::*;
use watch::*;

pub use surface::{
    ADVANCED_VERBS, PUBLIC_VERBS, REMOVED_SPELLING_RELEASE, Resolution, help_text, public_forms,
    removed_spelling_message, resolve, wants_help,
};

const RESOURCES_RECONCILE_USAGE: &str =
    "usage: aethyme broker advanced resources reconcile <lease-id> --confirm <generation> [--json]";
const OPERATIONS_RECONCILE_USAGE: &str = "usage: aethyme broker advanced operations reconcile \
     --operation <id|host-operation-id> (--outcome <succeeded|failed> | --inspect-remote) --reason <text> [--json]";
const OPERATIONS_SHOW_USAGE: &str =
    "usage: aethyme broker advanced operations show <id|host-operation-id> [--json]";
const OPERATIONS_STATS_USAGE: &str = "usage: aethyme broker advanced operations stats [--repo <canonical-id>] [--limit <n>] [--json]";
const EXEC_STATUS_USAGE: &str = "usage: aethyme broker advanced exec-status --session <id> [--receipt <32-hex-id> | --limit <1..100>] [--json]";
const ADVISORIES_SHOW_USAGE: &str = "usage: aethyme broker advanced advisories show <id> [--json]";
const ADVISORIES_ACK_USAGE: &str = "usage: aethyme broker advanced advisories ack <id> [--json]";
const ADVISORIES_SUPPRESS_USAGE: &str =
    "usage: aethyme broker advanced advisories suppress <id> [--json]";
const INTEGRATION_RECONCILE_USAGE: &str = "usage: aethyme broker advanced integration reconcile \
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
  aethyme broker readiness [--require <conflict-only|agent-ready|parallel-ready>] [--json]
      Interpret deterministic repository, broker, agent-context, validation,
      parallel-execution, graph, and upgrade facts into a typed readiness
      report. Inspection is offline and strictly read-only. By default every
      complete report exits zero; --require makes unmet readiness a CI failure.
  aethyme broker readiness plan [--repo <path>] [--local-only] [--resolution-file <path>] [--diff|--json]
  aethyme broker readiness apply [--repo <path>] [--local-only] [--resolution-file <path>] --confirm <plan-sha256> [--json]
  aethyme broker readiness recover [--repo <path>] --plan <plan-sha256> [--json]
      Plan exact safe repository repairs from committed HEAD, review their
      local diff, then apply only the confirmed digest through the shared
      repository-upgrade transaction. Recovery is explicit after interruption.
      There is no immediate --fix mode.
  aethyme broker scaffold [--json]
      Deterministic setup: ONLY what the broker needs, with content that
      is identical for every repo (config.toml skeleton, .gitignore
      block, broker database). Never overwrites. Certify + scaffold are
      the 'always exactly the same' pair — one reads, one writes.
  aethyme broker gates draft [--json]
      Adaptive (NOT scaffolding): sniff this repo's manifests and draft
      a gates.toml. Output depends on the repo — review it, then run
      certify.
  aethyme broker gates doctor [--probe] [--only <gate>] [--json]
      Inspect exact-HEAD gate quality without changing enforced selection.
      --probe explicitly runs all or one selected gate in a disposable
      detached worktree with ephemeral cache evidence and mutation capture.
  aethyme broker adopt [<path>] [--task <text>] [--short-name <name>] [--path <repo-path>]... [--agent <name-and-email>] [--repo-name <name>] [--tab-name <name>] [--ai-provider <provider>] [--reuse [--sync-integration]|--replace-stale] [--take-over] [--allow-main-checkout] [--json]
      Register an existing worktree (attach-first). Defaults to the
      current directory. A new session on the repository's main checkout is
      refused unless the operator explicitly passes --allow-main-checkout;
      use `broker start` for an isolated worktree. If the worktree already has a session:
      --reuse points it at a follow-up task with a fresh baseline and
      reports its relation to the current integration tip;
      --sync-integration requires --reuse and first fast-forwards a clean
      session worktree to the exact integration tip;
      --replace-stale closes it and registers fresh; policy may reclaim known ignored build artifacts;
      neither flag = error listing your options. Every --path is validated
      and claimed explicitly in the same transaction as create/reuse.
  aethyme broker close --session <id> [--abandon --reason <why>] [--json]
      Low-level close. Retains the checkout and branch; policy may reclaim
      known ignored build artifacts. Does not check whether commits were
      submitted. Where the repository sets [delivery] push_session_branches,
      refuses while the session holds commits no remote has, unless
      --abandon --reason records that they are being left behind. Prefer
      finish for normal lifecycle use.
  aethyme broker heartbeat --session <id> [--json]
      Renew the exact current holder's task lease for long-running work.
      Guarded session commands renew it automatically; send this at least once
      every 24 hours during a quiet task.
  aethyme broker finish --session <id> [--timeout <1..86400 seconds>] [--keep-worktree] [--abandon --reason <why>] [--json]
      Higher-level lifecycle close: closes only when the session has no
      dirty WIP and no committed work waiting for submit/promotion. Where
      the repository sets [delivery] push_session_branches, also refuses
      while HEAD has commits no remote holds; --abandon --reason closes
      anyway and records a broker.session.abandoned_unpushed event. If it
      is not safe, prints the next command; suggests cleanup only when
      cleanup would pass without --force. Git checks share a 10-second
      deadline by default; --timeout changes it, and an expiry names the
      timed-out check with safe status/handoff guidance. Successful closure atomically
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
  aethyme broker quality-report plan <path> --repo <owner/name> --pr <number> [--json]
  aethyme broker quality-report publish <path> --repo <owner/name> --pr <number> --session <id> [--json]
      Validate and idempotently publish a revision-bound, redacted quality
      report as the broker-owned neutral/advisory PR summary. Publication is
      opt-in and uses the coordinated GitHub operation lane; it never changes
      gate selection or CI certification.
  aethyme broker issues upsert --repo <owner/name> --fingerprint <key> --title <title> --body-file <path> [--label <label>]... [--session <id>] [--json]
  aethyme broker issues upsert --repo <owner/name> --fingerprint <key> --resolve [--body-file <path>] [--session <id>] [--json]
      Keep one issue per automation outcome (#1238). The first failure opens
      an issue carrying a hidden <!-- aethyme-outcome:<key> --> marker; a
      repeat comments on it at most once per issue_comment_interval_minutes;
      --resolve comments and closes it. Opt-in on the default branch:
      [automation] issues = allow, issue_labels and issues_per_day. Denied,
      unlisted-label and over-budget requests write nothing and are recorded
      locally. Writes are journaled before they run; with --session they go
      through the coordinated GitHub operation layer.
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
  aethyme broker start --task <text> [--short-name <name>] [--base <ref>] [--pull-request <number>] [--path <repo-path>]... [--agent <name-and-email>] [--repo-name <name>] [--tab-name <name>] [--ai-provider <provider>] [--json]
      Create a broker-managed worktree + branch and register a session,
      atomically claiming every reviewed --path, but do not spawn a process.
      Prefer this over adopting the main
      checkout for agent work; it isolates the git index and worktree.
      --agent as in adopt (see above).
  aethyme broker start-agent --task <text> --cmd <command> [--short-name <name>] [--pull-request <number>] [--agent <identity>] [--repo-name <name>] [--tab-name <name>] [--ai-provider <provider>] [--json]
      Create a worktree + branch and spawn <command> in it (sh -c),
      logging to .aethyme/logs/.
  aethyme broker prepare status --session <id> [--json]
      Inspect repository-declared dependency preparation for one worktree.
      This is read-only and reports the exact remediation when preparation is
      absent, stale, interrupted, failed, or invalid.
  aethyme broker prepare --session <id> [--offline] [--wait <duration>] [--json]
      Explicitly run the repository's .aethyme/prepare.toml commands in the
      session worktree. Shared caches are serialized host-wide; no command is
      ever run automatically by start or adopt. --offline refuses any step
      without a declared offline_command.
  aethyme broker advanced worktree-root [--json]
      Resolve the scanner-safe external root used by future broker starts.
      Read-only: reports the preferred location, repository key and
      fallbacks without creating any directory. A [worktrees] root in
      .aethyme/config.toml supersedes the per-user default while it exists,
      is outside the repository and has worktrees.min_free_bytes free;
      otherwise starts use the default and say why.
  aethyme broker advanced worktree-root binding plan <exact-root> <keyed_container|direct_root> [--json]
      Review an existing legacy root before binding it to this repository.
      Planning does not discover, create or change a root.
  aethyme broker advanced worktree-root binding apply <exact-root> <keyed_container|direct_root> --confirm <sha256> [--json]
      Bind only the reviewed legacy root if every planned identity still matches.
  aethyme broker advanced worktree-root recovery plan [--json]
      Inspect an incomplete fresh-root reservation and its durable prefixes.
  aethyme broker advanced worktree-root recovery apply --confirm <sha256> [--json]
      Complete only the fresh-root reservation bound to the reviewed digest.
  aethyme broker worktrees [--measure] [--json]
      List every worktree on this host with its repository, size, age and
      what removing it would lose (uncommitted, unpushed, recoverable).
      Read-only. Sizes come from recorded measurements, then walks within a
      10 s budget; a row not measured in time reports size unmeasured and the
      totals are floors. --measure walks every checkout instead.
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
  aethyme broker leases explain [<paths...>] [--json]
      Each active lease (or those overlapping <paths>) with its liveness:
      active, idle, stale or unknown, bound to the holder process, with the
      evidence. A holder gone past [leases] stale_grace_minutes no longer
      conflicts but is still listed. Read-only.
  aethyme broker leases request-release <path> --session <id> --reason <why> [--json]
      Ask every other session holding a lease on <path> to release it. The
      holder sees the request in broker status with ack/decline commands. A
      holder gone past [leases] stale_grace_minutes is granted against at
      once; one whose liveness is unknown never is. This is advisory and does
      not block edits in separate worktrees; inspect the holder's task and
      relevant diff before requesting coordination.
  aethyme broker leases ack <request-id> --session <holder> [--json]
      Acknowledge a release request: the holder's leases on the path are
      released.
  aethyme broker leases decline <request-id> --session <holder> --reason <why> [--json]
      Decline a release request; the leases stay.
  aethyme broker leases wait <path> --session <id> [--timeout <seconds>] [--json]
      Explicitly block this command until no other session holds <path> (exit 0, released), the
      broker granted it from a stale holder (exit 10), a request was declined
      (exit 11), the requester session closed and cancelled the request while
      the path remains held (exit 13), or --timeout (default 300) ran out (exit 12).
  aethyme broker leases export (--session <id> | --entry <id>) [--limit <n>] [--json]
      Export bounded, redacted lease ownership and deterministic routing
      categories from committed [leases.routing] configuration. Includes
      historical released and expired rows for the selected session. Never
      refreshes leases or writes broker state or command telemetry.
  aethyme broker leases release <path> --session <id> [--json]
      Release an explicit claim.
  aethyme broker ownership [list] [--json]
      List named operations a live session declared it is driving, with each
      holder's liveness and last coordinated operation.
  aethyme broker ownership claim <name> --session <id> --reason <text> [--json]
      Declare this session is driving <name>, e.g. a release.
      Refused only while another session holds it and is working; a quiet
      holder's claim is taken over and the report names it. Finishing the
      session ends the claim.
  aethyme broker ownership release <name> --session <id> [--json]
      Release this session's claim on <name>.
  aethyme broker resources plan <request.json> [--json]
      Read-only host-wide availability estimate for a typed resource bundle.
      Never reserves a port, namespace, capacity slot, or exclusive key.
  aethyme broker resources explain <request.json> [--json]
      Read-only diagnosis joining each blocked resource to its exact lease,
      holder process, liveness, and wait/reconcile guidance.
  aethyme broker resources pressure [--json]
      Read-only inactive-agent diagnosis, grouped by recorded holder identity.
      RSS sums are observational, shared pages may repeat; swap is unmeasured.
      Unknown evidence stays null. Never signals processes or reaps resources.
  aethyme broker resources acquire <request.json> [--wait <duration>] [--grant-out <path>] [--json]
      Atomically reserve the full bundle. --wait bounds contention retries.
      --grant-out atomically creates a mode-0600 private grant and keeps the
      ownership token out of command output.
  aethyme broker resources run <request.json> [--wait <duration>] [--cleanup-command <shell>] [--json] -- <command> ...
      Store v2 holds capacity before spawn; fresh opens by older binaries fail.
      Supervised request IDs are one-shot. SQLite fences already-open old
      writers from releasing/reaping held allocations or exceeding the pool
      limit. Any cleanup must succeed, and the child and optional cleanup
      groups must be observed absent; otherwise reconcile the exact
      generation after reviewing cleanup.
  aethyme broker resources artifact-run <path> --session <id> [--wait <duration>] [--cleanup-command <shell>] [--json] -- <command> ...
      Hold a strict cooperative claim on one generated file through command
      and cleanup. Other sessions cannot take it over merely because of idle
      activity. Run from the named session's own worktree root.
  aethyme broker resources renew <grant.json> --ttl <seconds> [--json]
  aethyme broker resources release <grant.json> [--json]
      Renew or release with the exact grant returned by acquire. Tokens are
      read from the file rather than command arguments or broker telemetry.
  aethyme broker resources list [--all] [--json]
      Read-only inventory. Ownership tokens are never included.
  aethyme broker resources reap [--dry-run] [--json]
      Reclaim ordinary capacity from quarantined leases whose holder is gone;
      supervised capacity holds remain until clean release or reconciliation.
      --dry-run projects capacity reconciliation without changing registry rows.
  aethyme broker resources reconcile <lease-id> --confirm <generation> [--json]
      Release an expired, quarantined allocation after reviewing host cleanup.
      The generation confirmation fences stale cleanup commands.
  aethyme broker console [status|list] [--json]
      Show the exact revision of this checkout and list consoles serving this
      repository right now, including each marker's branch, commit, worktree,
      port, and integration relation. Each console also reports whether its
      recorded checkout still exists and whether its supervisor is alive; a
      stale console names the `console stop` command that retires it.
      Read-only; reserves nothing.
  aethyme broker console stop <lease-id> [--force] [--dry-run] [--wait <duration>] [--json]
      Send SIGTERM to a console's supervising `console run`, which stops the
      server's process group, releases the lease, and removes the marker.
      Refuses a console that is not stale without --force, and never
      signals a holder whose lease expired, whose console has no verified
      marker, or whose pid cannot be proven to be the lease's supervisor.
      Waits up to --wait (default 10s) for the lease to be released.
  aethyme broker console plan [--allow-parallel] [--json]
      Show exactly what a console launch would reserve under the current mode
      without reserving it.
  aethyme broker console run [--wait <duration>] [--allow-parallel] [--json] -- <command> ...
      Run a dev server under the repository's console mode. `singular` takes
      one exclusive key and one pinned port, so a second launch anywhere on
      this host is refused with the console that already holds it.
      `--allow-parallel` is an explicit testing escape hatch: it keeps a
      registry lease and allocates another port from the configured range.
      `per_worktree` takes a distinct port, namespace, and one slot from a
      bounded pool, so worktrees run side by side without exhausting the host.
      `unmanaged` reserves nothing and executes directly. Allocations reach
      the command as AETHYME_RESOURCE_PORT, AETHYME_RESOURCE_NAMESPACE, and
      AETHYME_RESOURCE_SLOT. Managed commands also receive
      AETHYME_CONSOLE_MARKER and AETHYME_CONSOLE_MARKER_DIGEST; the marker is
      removed after clean shutdown. Configure with [console] in
      .aethyme/config.toml: mode, port, port_end, pool_limit, ttl_seconds.
  aethyme broker exec --session <id> [--non-interactive] [--timeout <1..86400 seconds>] [--json] -- <command>
      Run a command in the session worktree, then fail if it creates or
      modifies dirty paths outside explicit leases or in adoption-time
      foreign files. Exports AETHYME_TEST_DB_SUFFIX=s<id>-exec.
      Supervised execution has a 1800-second deadline by default; --timeout
      overrides it. On expiry, the supervisor verifies process-group cleanup
      before the ownership audit, and JSON reports command_timed_out as true.
      --non-interactive selects supervised process-group execution even when
      a terminal is available. This is a caller assertion, not TTY isolation:
      stdio routing stays the same as the default, and the child may still open
      the terminal. Use it only for commands that do not need terminal
      interaction; terminal reads or job-control operations may stop or fail.
      Automatic legacy terminal execution has no deadline; an explicit
      --timeout is refused in that mode. Combine it with --non-interactive
      only for a command that does not need terminal interaction.
      If the supervised helper cannot start, the child is not dispatched.
  aethyme broker commit --session <id> [--non-interactive] [--timeout <1..86400 seconds>] [--json] -- <git-commit-args>
      Run a local Git commit for the session's own checked-out branch. It
      refuses inherited Git routing/config overrides, hook-bypass options,
      incomplete path inventories, and paths outside current session ownership.
      The command uses normal Git hooks and records a guarded-exec receipt.
  aethyme broker exec-status --session <id> [--receipt <32-hex-id> | --limit <1..100>] [--json]
      Read guarded-exec outcomes for this repository and session. A starting
      or running receipt is not process-liveness proof; this command never
      signals, cleans up, or changes session state.
  aethyme broker git --session <id> [--repo <owner/name>] [--scope <scope>] [--effect <read|write|destructive>] [--reason <text>] [--destructive [--cross-session <id>]] [--no-wait|--queue-timeout <seconds>] [--json] -- <git-args>
      Run Git through the durable operation coordinator. Remote Git commands
      require an exact --repo. Repository writes are serialized, journaled,
      and fail closed after a crash with an unknown remote outcome.
      A write queues for the repository lock and is recorded while it waits, so
      `operations list` shows it. Use --no-wait to refuse rather than queue, or
      --queue-timeout <seconds> to give up after a bounded wait.
      A destructive write to a branch that belongs to another live session is
      refused unless --cross-session names that session.
  aethyme broker gh --session <id> --repo <owner/name> [--scope <scope>] [--effect <read|write|destructive>] [--reason <text>] [--destructive [--cross-session <id>] [--ref-write-acknowledged]] [--no-wait|--queue-timeout <seconds>] [--json] -- <gh-args>
      Run GitHub CLI through the same repository coordinator. The broker sets
      GH_REPO from the exact target and never persists command output or
      secret-bearing argument values. After a successful `gh pr merge`, it
      refreshes the tracked target and removes a fully landed integration
      layer; mixed or uncertain work remains unchanged with recovery guidance.
      A command that can delete or move a branch ref (pr merge|close
      --delete-branch, update-branch --rebase, repo sync --force, api ref
      writes, GraphQL mutations, aliases) is checked against the exact branch
      it touches; one the broker cannot pin to a branch is refused when a
      shared branch could be involved, even with --destructive
      --ref-write-acknowledged.
  aethyme broker merge-chain --session <id> --repo <owner/name> --reason <text> <pr>... [--merge-method <merge|squash|rebase>] [--poll-seconds <n>] [--checks-timeout <seconds>] [--main-timeout <seconds>] [--gates-workflow <file> [--dispatch-after <seconds>]] [--dry-run] [--json]
      Land pull requests one at a time, in order. For each: mark it ready,
      update it onto the base when behind, wait for the latest run of every
      check on that exact head, merge with --match-head-commit, then wait for
      the base branch's runs on the merge commit (dispatching --gates-workflow
      when none appears). Stops at the first failure and names the safe next
      action; every write is a journaled `gh` operation. Re-running resumes:
      merged pull requests are skipped and the last one is verified again.
  aethyme broker operations list [--limit <n>] [--before <id>] [--session <id>] [--status <status>] [--repo <canonical-id>] [--provider <git|github>] [--json]
      List a filtered newest-first page of the durable operation journal.
      `operations` without `list` is a compatibility alias during deprecation.
  aethyme broker operations show <id|host-operation-id> [--json]
      Show one exact durable operation and its reconciliation state, evidence,
      write barrier, and complete recovery commands when inspection is required.
  aethyme broker operations reconcile --operation <id|host-operation-id> (--outcome <succeeded|failed> | --inspect-remote) --reason <text> [--json]
      Record a broker inspection proposal for an exact-ref Git push, or record
      an operator-asserted outcome. Inspection never clears the write barrier.
  aethyme broker operations stats [--repo <canonical-id>] [--limit <n>] [--json]
      Read bounded lock-hold, queue-wait, queue-depth, and known-unrelated
      contention measurements. Older operations without timing data are
      reported as unmeasured; this command never changes lock policy.
  aethyme broker blockers [--json]
      Read-only: every current blocker across broker.db, the host operation
      and resource ledgers, gate pidfiles, and conflict notices, each with a
      stable id (op:<n>, hostop:<hex>, resource:<id>, lease:<id>,
      gatecache:<gate>@<tree>, pidfile:<session>-<gate>, action:<session>),
      its cause, and the one command that clears it.
  aethyme broker unblock <id> [--outcome <succeeded|failed>] [--reason <text>] [--confirm <generation>] [--json]
      Clear one blocker through its store's recovery path. Outcome-unknown
      operations need --outcome and --reason from an operator who inspected
      the remote; cached failing gate verdicts need --reason; unclosed
      resource leases need --confirm <generation>. Refusals exit 3 and name
      the flag they need.
  aethyme broker advisories list [--all] [--json]
      List outstanding non-blocking advisories newest-first. --all includes
      acknowledged, suppressed, and resolved history. Deliberate inventory
      refreshes bounded maintainer recommendations; the broker database is authoritative.
  aethyme broker advisories show <id> [--json]
      Show one exact advisory with paths, evidence, integration provenance,
      creation time, and resolution state.
  aethyme broker advisories ack <id> [--json]
      Acknowledge one advisory idempotently and atomically refresh
      .aethyme/broker-advisory.md from the remaining outstanding rows.
      Promotion/lease advisories repeat on session commands, after
      post-commit, and before uncached gates whose cost exceeds 1; they
      remain informational and never alter command or promotion outcomes.
  aethyme broker advisories suppress <id> [--json]
      Suppress a maintainer recommendation across later evidence samples.
      Session-facing coordination advisories cannot be suppressed.
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
  aethyme broker gates incidents list [--json]
  aethyme broker gates incidents confirm <failed-run-id> <test-id>... --session <id> [--pr <n>] [--json]
  aethyme broker gates incidents resolve <incident-id> <passing-run-id> [--json]
  aethyme broker gates incidents handoff <incident-id> <expected-owner> --session <new-owner> --reason <text> [--json]
      Record or resolve bounded advisory ownership of a confirmed base failure;
      never skips gates. Confirmation requires exact base/definition/profile evidence.
  aethyme broker gates manifest [--head <ref>] [--json]
      Emit a versioned content-free gate-selection manifest from exact
      committed policy (default head: HEAD). Commands are never included.
  aethyme broker gates scope --base <ref> --head <ref> [--json]
      Evaluate the shared path selector for two exact commits using the
      gates.toml committed at head. Read-only; semantic hints stay advisory.
  aethyme broker gates affected --session <id> [--json] [--timings]
      Show which gates the session's diff selects and why, warning on stderr
      when a phase exceeds its 5-second budget. --json prints the selection
      array; --json --timings prints an object that adds per-phase timings.
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
  aethyme broker hooks snippet <pre-commit|post-commit|pre-push> [--json]
      Print a copy-pasteable external-manager fragment generated by the same
      renderer as the managed shim. JSON includes the binary, subcommand, and
      arguments. Unknown hook names are rejected.
      (hooks pre-commit / hooks post-commit / hooks pre-push are internal entry
      points the installed shims call — not for direct use.)
  aethyme broker trust [--repo <path>] [--json]
      Show every gate and prepare command this repository defines, then
      record their exact policy digest as trusted on this machine. Until a
      policy is trusted, submit, gates run, the pre-commit hook and prepare
      refuse (exit 3) before running any repository-defined command; a policy
      change needs trusting again. Requires an interactive terminal on stdin,
      so an agent cannot approve itself. Repositories that already had gate
      history are trusted with their current policy automatically.
      AETHYME_TRUST_NONINTERACTIVE_FOR_TESTS=1 is a test-only escape.
  aethyme broker trust status [--repo <path>] [--json]
      Read-only: the checkout's and integration tip's policy digests, whether
      each is trusted, and the approved digests on record.
  aethyme broker pr check [--target <branch>] [--pr <number>] [--agent <name>] [--dispatch] [--cmd <command>] [--json]
  aethyme broker watch pr start --session <id> --repo <owner/name> --pr <number> [--events <comments,reviews,checks>] [--seconds <15..3600>] [--json]
  aethyme broker watch pr monitoring <activate|deactivate|status> --session <id> [--json]
      Opt this session in to PR monitoring. Off by default: while active, a
      pull request opened through `broker gh` starts a watch automatically, and
      the scheduled poller may deliver its review activity to this session.
  aethyme broker watch pr list [--all] [--json]
  aethyme broker watch pr show|poll|pause|resume|stop --id <watch-id> [--json]
  aethyme broker watch pr tick [--limit <1..100>] [--json]
  aethyme broker watch pr service plan --broker-bin <absolute-router> [--seconds <120..3600>] [--json]
      Read-only macOS service plan. Does not install or activate a scheduler.
  aethyme broker watch pr service install --broker-bin <absolute-router> --confirm <plan-digest> [--seconds <120..3600>] [--json]
  aethyme broker watch pr service activate --confirm <installed-plan-digest> [--json]
  aethyme broker watch pr service deactivate --confirm <installed-plan-digest> [--json]
  aethyme broker watch pr service status [--json]
      macOS only: install exact service files without loading a scheduler.
  aethyme broker watch pr batches --id <watch-id> [--all] [--json]
  aethyme broker watch pr ack --id <batch-id> --outcome <addressed|stale|non-actionable|superseded> --reason <text> [--json]
      Persist a metadata-only PR watch and inspect it with one-shot polling.
      Aethyme records provider ids, authors, states, URLs and timestamps, but
      never comment/review bodies. `tick` performs one bounded foreground
      scheduling pass; the broker never starts a background poller.
  aethyme broker watch repo start --session <id> --repo <owner/name> [--events <opened,ready_for_review,reopened>] [--include-drafts] [--include-existing] [--exclude-authors <login,login>] [--auto-watch] [--seconds <15..3600>] [--json]
  aethyme broker watch repo list [--all] [--json]
  aethyme broker watch repo show|events|pause|resume|stop --id <repo-watch-id> [--json]
      Watch every pull request of a repository: each `watch pr tick` lists its
      open PRs (metadata only) and records one event per PR and kind, opened,
      ready_for_review or reopened, exactly once. PRs already open at start do
      not fire unless --include-existing. --auto-watch also starts one per-PR
      watch for each new PR.
  aethyme broker deliveries subscribe --watch <id> --adapter <name> --target <opaque-id> [--policy <notify|resume|review-and-push>] [--json]
  aethyme broker deliveries subscribe --repo-watch <id> --adapter <name> --target <opaque-id> [--policy <review|notify>] [--json]
      A repository-watch subscription delivers one prompt per event. `review`
      (the default) asks the session to code-review the PR; the prompt comes
      from `[watch.prompts.review] body` in .aethyme/config.toml when set.
  aethyme broker deliveries list [--adapter <name>] [--all] [--json]
  aethyme broker deliveries claim --adapter <name> --worker <id> [--seconds <15..900>] [--json]
  aethyme broker deliveries resolve-tab --session <id> [--tabs-file <path>] [--json]
  aethyme broker deliveries dispatch --adapter chau7 --worker <id> [--tabs-file <path>] [--seconds <15..900>] [--json]
      Claim one delivery and decide what to do with it: send to a resolved tab,
      defer while that tab is mid-turn or absent, or abandon when it cannot be
      identified. Deferred and abandoned outcomes are completed for you; a send
      is left open so the caller completes it after the transport succeeds.
      Resolve which Chau7 tab is running a session, from a `tab_list` snapshot
      on stdin or --tabs-file. The broker never calls Chau7 itself; an adapter
      supplies the snapshot and performs the transport. Ambiguity refuses.
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
  aethyme broker review plan [--base <ref>] [--pr <number>]
      Print the reviews this repository's policy would ask for on the current
      change, who would perform them, and what would go on the pull request.
      Reads git and .aethyme/config.toml, needs no session, and performs
      nothing. Every review table is off by default, so an unconfigured
      repository plans nothing.
  aethyme broker review run --session <id> --repo <owner/name> --pr <number> [--base <ref>] [--tabs-file <path>] [--from-provider] [--dry-run]
      Perform one tick of the review router for one pull request: decide as
      `review plan` does but with the spent reviews read from the ledger, the
      in-flight reviews read from a Chau7 tab snapshot, and the pull request
      read with read-only gh. First gives up on any review whose reviewer has
      not reported inside the route's stale_after_minutes, so a dead reviewer
      cannot hold a slot forever. Records each request before it is handed out,
      then performs the GitHub writes through the coordinated lane and prints
      the Chau7 spawns for an adapter to start. --dry-run stops after the plan.
  aethyme broker review tick --session <id> --repo <owner/name> [--limit <count>] [--tabs-file <path>] [--dry-run]
      Run `review run --from-provider` over the open pull requests of one
      repository, oldest first, and print one report for the sweep. This is
      the whole scheduler: the broker starts no background poller, so a
      cron entry, a CI step, or a person runs this bounded pass. A pull
      request that fails is recorded in the report and skipped, never fatal,
      so one broken pull request cannot stop the rest of the sweep.
  aethyme broker review ledger --repo <owner/name> [--pr <number>] [--json]
      Print the review ledger: every review request or provider completion, for
      which heads, through which backend, and how it ended. This is the answer
      to \"why was there no review\" -- read-only, needs no session.
  aethyme broker review state --repo <owner/name> --pr <number> --type <review-type> --state <state> [--head <sha>] [--note <text>] [--completed-for-commit <sha> --verdict <verdict> --reviewer-provider <provider> [--reviewer-model <model>]] [--json]
      Report what became of one requested review: running, satisfied, failed,
      recorded, or abandoned. A satisfied report includes the typed completion
      commit, verdict, and reviewer identity. Without --head, an exact
      completion-head match is used or a new unsolicited provider result is
      recorded. The broker decides and records; whoever performs the review
      closes the row here, which is what drains the router's concurrency slots.
      --head targets a superseded request.
  aethyme broker review waive --repo <owner/name> --pr <number> --type <review-type> --head <sha> --reason <text> [--agent <name-and-email>] [--json]
      Excuse one review dimension at one head, on the record. Waives exactly
      the named type at the named commit and nothing else: a new head has no
      waiver, so it expires without anyone withdrawing it. --reason is stored
      as text, not as a digest, because it is addressed to whoever later asks
      why this dimension is green. Never use `review state --state satisfied`
      for this -- that claims a review ran.
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
  aethyme broker advanced review-thread reply <thread-id> --session <id> --repo <owner/name> --pr <number> --message <text> --reason <text> [--no-wait|--queue-timeout <seconds>] [--json]
      Reply to one inline review thread through a narrowly allowlisted GraphQL
      mutation after verifying its repository and pull request. The broker
      serializes writes for that thread and redacts the reply body from its journal.
  aethyme broker advanced review-thread resolve <thread-id> --session <id> --repo <owner/name> --pr <number> --reason <text> [--no-wait|--queue-timeout <seconds>] [--json]
      Verify and resolve one inline review thread through the same coordinated lane.
  aethyme broker submit --session <id> [--no-cache] [--verify-only] [--json]
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
      Session holder: a session is held by the agent process (claude,
      codex, or AETHYME_AGENT_PID) that started or adopted it. submit,
      push, sync, finish, close, git, gh and adopt --reuse refuse another
      live agent; --take-over moves the session to the caller and records
      a session.holder_bound event. Unidentified callers are not refused.
  aethyme broker push --session <id> [--pr] [--json]
      Publish this session's own agent/* branch to the default branch's
      remote, and nothing else. Commit, then push, as often as you like:
      work that exists only in a worktree is lost with it. A branch that only
      moves forward is pushed plainly; a rebased one is replaced only with a
      lease on the oid this broker last pushed for the session. Requires
      `[delivery] push_session_branches = true` in .aethyme/config.toml on
      the default branch. --pr opens a new pull request using the trusted
      default branch's `[broker.pr] open_as = \"draft\" | \"ready\"` setting
      (default: draft); an existing pull request is reported unchanged.
  aethyme broker sync --session <id> [--json]
      Bring this session up to the freshly fetched default branch when it
      is safe: the tree must be clean and nothing in progress. An unpushed
      branch is rebased; a published one has the default branch merged in,
      so a pull request's history is never rewritten. If catching up would
      conflict, nothing changes and the conflicting paths are listed. Only
      the local session branch moves; nothing is pushed, so follow a sync
      with `broker push`. Run it before resuming a reused session and
      whenever `push` reports the branch is behind the default branch.
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
  aethyme broker queue [--active] [--json]
      `--active` lists only entries that can still change (submitted,
      simulating, verified, conflict) — the cheap way to watch a submit in
      flight without paying for the whole inventory on every poll.
  aethyme broker queue history [--limit <n>] [--before <id>] [--json]
      The bare command remains the compatibility inventory. `history` is a
      bounded newest-first terminal page with a stable next_before_id cursor.
      Show the merge queue.
  aethyme broker promote --entry <id> [--json]
      Manual-mode only: advance the local integration branch to a verified
      entry's merge commit; other in-flight entries are re-simulated.
      Promotion stays local; publish through `broker ship plan --entry <id>`.
  aethyme broker ship plan --entry <id> [--only] [--delivery <local_main_merge|pull_request>] [--json]
      Build a delivery plan through an exact promoted entry: resolve the
      trusted delivery policy, divergence recommendation, publication prefix,
      remote freshness, proposed route, and local-main safety. --only selects
      one independent entry and gates origin/main plus its changes in an
      exact-tree checkout. Planning never moves refs; --only may record gate
      results and cache entries. After publication it reconciles the remaining
      promoted entries and re-verifies every replay whose tree changed.
  aethyme broker ship execute --entry <id> --confirm <full-publication-sha> [--only] [--delivery <local_main_merge|pull_request>] [--plan <sha256>] [--sync-main] [--break-glass --reason <authorization>] [--json]
      Revalidate and execute the reviewed delivery route. Local-main delivery
      uses a clean fast-forward merge; pull-request delivery pushes a
      deterministic branch and opens or verifies one exact GitHub PR.
      --only requires the plan SHA-256 produced by `ship plan --only`.
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
  aethyme broker status [--json] [--compact] [--summary] [--refresh]
      Default text is compact. --compact --json returns a versioned summary
      that groups advice and marks omitted details and deferred checks; those
      checks are unknown, not clean. Full --json retains per-session detail;
      session records use `agents[].id` for every --session flag. The legacy
      --summary --json shape is unchanged; --summary text remains brief.
      --refresh explicitly performs the expensive full lease, conflict and
      cleanup audit; use it when needed.
  aethyme broker events [--since <id>] [--kind <prefix>] [--follow] [--json]
      Show the append-only event log (see docs/events-contract.md).
      --kind filters by prefix (e.g. merge. or lease.overlap); --follow
      waits for database file notifications and retries transient read errors.
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
  aethyme broker doctor plan [--json]
      Every debris item across stores, with its owner and whether that owner
      is gone, live or unknown: blockers (outcome_unknown operations,
      quarantined leases, ...), stale session worktrees, and the containers
      and volumes gate commands labelled from AETHYME_GATE_LABELS. Read-only;
      prints the digest `doctor apply` needs. A store it could not read is
      listed, never treated as clean.
  aethyme broker doctor apply --confirm <sha256> [--json]
      Remove the labelled containers and volumes a reviewed plan marked
      removable: only those whose gate run is verifiably over, judged again
      now. A plan that changed since review removes nothing. Blockers and
      worktrees are never removed here; their own commands are listed.
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
  aethyme broker cleanup resolve <session-id>... --archive [--batch] [--retire] [--allow-adopted] [--confirm <sha256>] [--json]
      Preserve, then remove, a closed session's worktree that cleanup refuses
      because it is dirty, holds pending commits, or cannot be proved landed.
      Without --confirm it prints what would be archived and a digest. With it,
      it writes a recovery archive under the host state directory (a bundle of
      commits no delivery target holds, staged and unstaged patches, untracked
      files Git does not ignore, a manifest and restore steps), reads every part
      back, and only then removes the worktree. A verification failure leaves
      the worktree in place. Discarding stays `cleanup <session-id> --force`.
  aethyme broker cleanup audit [--repo <path>] [--detail] [--json]
      Read-only, repository-scoped audit of every retained worktree, branch,
      Git worktree registration and unowned worktree-root directory. Judges
      committed work against a named snapshot of the default branch, preferring
      the fetched upstream over a stale local branch, and recognises
      fast-forward, merge, squash and rebased delivery. Reports tracked and
      untracked changes separately, a disposition (in_target, integration_only,
      remote_branch_only, worktree_only, dirty, live, missing_checkout_metadata, unknown_provenance),
      bytes, blocker and next action per item, and the cleanup apply digest,
      which binds the same target snapshot. --repo audits the repository at
      <path> instead of the current directory.
  aethyme broker main reconcile plan [--detail] [--resolution-file <path>] [--write-resolution-template <path>] [--json]
      Read-only classification of everything the local default branch carries
      that the integration branch does not. A commit counts as already
      represented when integration holds its content for every path it touched,
      which recognizes work that landed through a squashed promotion and whose
      SHA therefore differs. A clean default branch that is a strict ancestor
      of integration gets a `fast_forward` plan instead. Uncommitted tracked
      changes, a checkout on another branch, or any commit that cannot be
      proven represented refuse the apply.
  aethyme broker main reconcile apply --session <id> --confirm <sha256> [--resolution-file <path>] [--json]
      Move the local default branch onto integration after re-proving the
      reviewed plan. A reset creates a preservation ref at the pre-move tip
      first; a fast-forward runs `merge --ff-only` to the exact reviewed SHA
      and needs none. Never runs when anything would be lost.
  aethyme broker representation scan --session <id> [--json]
  aethyme broker representation status --session <id> [--json]
      Read-only: decide whether this session's work is already present on the
      default branch, by comparing its content against each commit the branch
      gained since the session branched. Ancestry is never consulted, so a
      provider-side squash under a new SHA is still found.
  aethyme broker representation record --session <id> --confirm <sha256> [--json]
      Record the reviewed landing so the session can finish. Re-proves the scan
      and refuses if anything moved since it was reviewed.
  aethyme broker promotion-record plan [--json]
      Read-only plan for integration commits that no promoted queue entry
      claims. A commit is recoverable when exactly one non-promoted entry
      recorded a merge tree equal to the commit's tree; ambiguous or
      unmatched commits are reported, never repaired.
  aethyme broker promotion-record apply --confirm <sha256> [--json]
      Restore the promoted record for every recoverable candidate in the
      reviewed plan. Re-proves each precondition, writes only status and
      commit details, and rebuilds the missing path exposures.
  aethyme broker reclaim plan [--session <id>] [--json]
  aethyme broker reclaim apply --confirm <sha256> [--session <id>] [--json]
      Report regenerable build output (target, node_modules, .venv, build,
      dist) in session worktrees, and remove only what was reviewed. An active
      session's artefacts are listed but never removed. Apply removes each
      reviewed path that is still reclaimable and leaves anything new for a
      later plan. --session scopes both to one session's worktree. Nothing
      here is recreated for you: a reclaimed worktree pays a cold build next
      time.
  aethyme broker gc plan [--include-active-gate-cache] [--full-scan] [--json]
      Report exact retention-eligible rows, runtime files, represented
      worktrees/refs, finished sessions' build caches, this repository's gate
      cache, estimated bytes, blockers, and (when complete) a stable plan digest. The newest
      gate cache of each kind is kept (active) unless
      --include-active-gate-cache is given; the next gate then rebuilds it
      from scratch. The default plan has a cooperative 10-second shared
      inspection budget;
      an incomplete plan has no apply digest. A nonempty recovery archive
      inventory is deferred by default because verifying its bundles can be
      expensive. --full-scan removes the default 10-second time budget and
      artifact-discovery entry cap; discovery still stops at six directory
      levels.
  aethyme broker gc apply --confirm <sha256> [--include-active-gate-cache] [--full-scan] [--json]
      Apply or resume the exact reviewed plan under an exclusive lock. A
      recovery journal makes interrupted row, file, and worktree cleanup safe.
      Pass the same --include-active-gate-cache and --full-scan options used
      for the plan. An existing journal resumes without a fresh plan scan.
  aethyme broker gc abandon --confirm <pending-digest> [--json]
      Archive a schema 2 or 3 pending journal without resuming or removing
      retained data. Requires a fresh plan review before later apply.
  aethyme broker gc forget-abandoned --confirm <archived-plan-digest> [--json]
      Forget the matching archive or its private interrupted staging files;
      retained project data is not removed. Archives are bounded to 32 files
      and 64 MiB total.
  aethyme broker gc artifact-use reconcile plan <exact-session-id> [--json]
      Review exact marker bytes, closed checkout aliases, registry identity
      history and PID/start/group absence for one retained checkout.
  aethyme broker gc artifact-use reconcile apply <exact-session-id> --confirm <sha256> [--json]
      Remove only markers re-proven quiescent against the exact plan digest.
      Uncertain processes and unmatched historical identities remain blocked;
      stopped applies report removed, retained and unknown markers; this never
      removes the checkout or its broker records.
  aethyme broker gc sweep [--json]
      Run the unattended build-artifact sweep now with its full budget
      (scaled up when disk is below the gate headroom), ignoring its daily
      cadence. It removes only git-ignored build caches of closed sessions;
      broker open spends at most 250 ms on the same sweep. Open-session caches
      require the reviewed gc reclaim path. Run again to continue a pass that
      paused at its budget.
  aethyme broker gc service plan [--json]
      Read-only macOS per-repository service plan for fixed `gc sweep --json`
      ticks every 900 seconds. Binds the immutable managed router/engine pair.
  aethyme broker gc service install --confirm <plan-sha256> [--json]
      Install exact service files without loading launchd.
  aethyme broker gc service status [--json]
      Observe service scheduler state and the bounded last-pass receipt.
  aethyme broker gc service activate --confirm <installed-plan-sha256> [--json]
  aethyme broker gc service deactivate --confirm <installed-plan-sha256> [--json]
      Load or unload the exact per-repository service. A changed runtime
      requires a reviewed plan and service generation refresh.
  aethyme broker gc service uninstall --confirm <installed-plan-sha256> [--json]
      Remove exact installed service files only after successful deactivation.
  aethyme broker storage [--json]
  aethyme broker storage plan [--json]
  aethyme broker storage apply --confirm <sha256> [--json]
      Inventory every host worktree root by reconciling disk directories,
      Git registrations, and repository session ledgers. Apply removes only
      exact reviewed orphan roots or stray directories.
  aethyme broker storage identity plan <host-operation-db> <exact-root> [--json]
  aethyme broker storage identity apply <host-operation-db> <exact-root> --confirm <old-sha256:new-sha256:owner-marker-sha256> [--json]
      Review then recover one legacy worktree-root identity after a mount
      identity change. The live root and repository origin must retain their
      inodes, owner marker, repository binding, and nonzero volume UUIDs; the
      confirmation binds the exact old and replacement payloads and owner
      marker bytes. This does not prove historical volume UUIDs.
  aethyme broker storage attribute [--apply] [--json]
      List host worktree roots with no ownership marker and whether this
      repository can prove it owns each one: every Git worktree under it is
      registered here, or its name is this repository's key. --apply writes
      the marker for those it can prove, removing nothing; review a new
      storage plan afterwards. A root it cannot prove is never marked.
  aethyme broker check-contract [--base <ref>] [--pr-body <file>]
                                [--commit-messages] [--merged-pr]
      Cross-process contract gate: refuse a diff that removes symbols
      listed in the consumers registry unless the PR body, the commits in
      <base>..HEAD, or (--merged-pr, via gh) the pull request GitHub
      associates with HEAD declare a contract decision. Run by CI and by
      the `cross-process-contract` gate. Exit 1 = undeclared contract
      change; the refusal lists every source it read.

Overlaps warn — they never block (v0 policy).
";

pub fn run(args: &[String]) -> u8 {
    run_with_mode(args, CompatibilityMode::Normal)
}

/// How the router permits this broker invocation to observe broker state.
/// Degraded repository compatibility uses `ReadOnlySnapshot`; ordinary
/// current-repository operation retains reconciliation behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompatibilityMode {
    Normal,
    /// Carries the router's explanation so a refusal can say why the command
    /// is read-only and what to run next, not merely that it is.
    ReadOnlySnapshot(ReadOnlyCompatibility),
}

/// Why the router chose a read-only snapshot for this repository, and the
/// action that restores normal (writing) broker commands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadOnlyCompatibility {
    /// The repository's compatibility state relative to this binary, e.g.
    /// "repository deployment schema 0 requires an embedded upgrade to schema 1".
    pub cause: String,
    /// The command that returns the repository to normal operation.
    pub remediation: Option<String>,
}

/// The one wording for a flag or command refused because it would write
/// broker state while the router only allows a read-only snapshot. It names
/// the cause, the action that lifts the restriction, and the read-only
/// alternative that works now.
fn read_only_refusal(parsed: &Parsed, refused: &str, snapshot_alternative: &str) -> UsageError {
    let mut message = format!(
        "{refused} writes broker state, so it is unavailable while this repository runs broker \
         diagnostics as a read-only compatibility snapshot"
    );
    if let Some(compatibility) = &parsed.read_only_compatibility {
        message.push_str(&format!(": {}", compatibility.cause));
        if let Some(remediation) = &compatibility.remediation {
            message.push_str(&format!(".\nTo restore it: {remediation}"));
        }
    }
    message.push_str(&format!(
        ".\nFor the recorded snapshot now: {snapshot_alternative}"
    ));
    UsageError::Message(message)
}

pub fn run_with_mode(args: &[String], mode: CompatibilityMode) -> u8 {
    let before = crate::cli_output::snapshot();
    let code = run_with_mode_inner(args, mode);
    crate::cli_output::exit_code_after_output(before, code)
}

fn run_with_mode_inner(args: &[String], mode: CompatibilityMode) -> u8 {
    let resolved = resolve(args);
    if let Some(refusal) = &resolved.refusal {
        return report_dispatch_error(args, crate::exit_status::USAGE, refusal);
    }
    // Help is answered before anything opens the broker or records a metric:
    // asking what a command does must never do any of it (Phase 4, P4.3).
    if wants_help(args) {
        return match help_text(args) {
            Some(text) => {
                out!("{}", text.trim_end());
                0
            }
            None => report_dispatch_error(
                args,
                crate::exit_status::USAGE,
                &format!(
                    "unknown broker subcommand {:?} — see aethyme broker --help",
                    args.iter()
                        .find(|arg| !arg.starts_with('-'))
                        .map(String::as_str)
                        .unwrap_or_default()
                ),
            ),
        };
    }
    if resolved.args.is_empty() {
        let text = if args.is_empty() {
            surface::public_help()
        } else {
            surface::advanced_help()
        };
        eprint!("{text}");
        return crate::exit_status::USAGE;
    }
    run_resolved_with_mode(&resolved.args, mode)
}

/// Dispatch an internal command produced by `resolve`, for trusted in-process
/// callers that have already applied the public-surface checks.
pub fn run_resolved(args: &[String]) -> u8 {
    run_resolved_with_mode(args, CompatibilityMode::Normal)
}

/// Share setup reads across the router preflight and routine status dispatch.
/// Only ordinary compact/summary observation is eligible; audits and mutations
/// retain fresh reads and their established initialization paths.
pub fn with_status_read_context<T>(args: &[String], operation: impl FnOnce() -> T) -> T {
    let args = if args.first().map(String::as_str) == Some("broker") {
        &args[1..]
    } else {
        args
    };
    if routine_status(args) {
        crate::status_reads::with_context(operation)
    } else {
        operation()
    }
}

fn routine_status(args: &[String]) -> bool {
    args.first().map(String::as_str) == Some("status")
        && args[1..].iter().all(|arg| {
            matches!(
                arg.as_str(),
                "--json" | "--json-output" | "--summary" | "--compact"
            )
        })
        && (!args
            .iter()
            .any(|arg| matches!(arg.as_str(), "--json" | "--json-output"))
            || args
                .iter()
                .any(|arg| matches!(arg.as_str(), "--summary" | "--compact")))
}

/// Dispatch an internal command with the router-selected compatibility mode.
pub fn run_resolved_with_mode(args: &[String], mode: CompatibilityMode) -> u8 {
    with_status_read_context(args, || run_with_status_context(args, mode))
}

fn run_with_status_context(args: &[String], mode: CompatibilityMode) -> u8 {
    let before = crate::cli_output::snapshot();
    let code = run_resolved_with_mode_inner(args, mode);
    crate::cli_output::exit_code_after_output(before, code)
}

fn run_resolved_with_mode_inner(args: &[String], mode: CompatibilityMode) -> u8 {
    // Top-level aethyme init --help and aethyme certify --help use the
    // internal spelling because those are canonical top-level commands.
    if wants_help(args) {
        return match surface::help_text_for_internal_command(args) {
            Some(text) => {
                out!("{}", text.trim_end());
                0
            }
            None => report_dispatch_error(
                args,
                crate::exit_status::USAGE,
                "unknown internal broker command",
            ),
        };
    }
    if args.is_empty() {
        eprint!("{}", surface::public_help());
        return crate::exit_status::USAGE;
    }
    let started = std::time::Instant::now();
    maybe_fast_forward_main_checkout(args, &mode);
    // Dispatched before the shared parser: the contract check is a CI/gate
    // entry point with its own flags (`--base`, `--pr-body`) and its own
    // exit-code contract (2 = bad invocation), and it deliberately records
    // no command metric — it runs on every submit and would swamp the
    // ledger with noise.
    if args.first().map(String::as_str) == Some("check-contract") {
        return crate::contract_check::run(&args[1..]);
    }
    let command_output = crate::cli_output::snapshot();
    let (code, record_outcome, error) = match run_inner(args, &mode) {
        Ok(()) => (0, true, None),
        Err(UsageError::Help) => {
            eprint!("{}", surface::public_help());
            (crate::exit_status::USAGE, false, None)
        }
        Err(UsageError::Message(message)) => {
            let code = report_command_error(
                args,
                crate::exit_status::FAILED,
                &message,
                crate::cli_output::output_attempted_since(command_output),
            );
            (code, true, Some(message))
        }
        Err(UsageError::Exit { message, code }) => {
            let code = report_command_error(
                args,
                code,
                &message,
                crate::cli_output::output_attempted_since(command_output),
            );
            (code, true, Some(message))
        }
        Err(UsageError::SilentExit(code)) => (code, true, None),
    };
    let code = crate::cli_output::exit_code_after_output(command_output, code);
    let internal_hook = args.first().map(String::as_str) == Some("hooks")
        && matches!(
            args.get(1).map(String::as_str),
            Some("pre-commit" | "post-commit")
        );
    if mode == CompatibilityMode::Normal && !internal_hook && !idle_delivery_poll(code) {
        if record_outcome {
            record_command_outcome(args, code, error.as_deref());
        }
        record_command_metric(args, code, started.elapsed().as_millis() as i64);
    }
    code
}

#[derive(Debug, PartialEq, Eq)]
enum ErrorRendering {
    Json(String),
    Text(String),
    Diagnostic(String),
}

pub fn json_requested(args: &[String]) -> bool {
    let mut args = args.iter();
    while let Some(argument) = args.next() {
        if argument == "--" {
            break;
        }
        if VALUE_FLAGS.contains(&argument.as_str()) {
            args.next();
            continue;
        }
        if matches!(argument.as_str(), "--json" | "--json-output") {
            return true;
        }
    }
    false
}

fn render_cli_error(
    args: &[String],
    code: u8,
    message: &str,
    output_attempted: bool,
) -> ErrorRendering {
    if json_requested(args) {
        if output_attempted {
            // Keep an already-started report as the only stdout document,
            // while preserving the failure reason and any recovery handoff.
            // This also covers a partial stdout write, where appending a
            // second JSON value could not repair the first one.
            return ErrorRendering::Diagnostic(message.to_string());
        }
        ErrorRendering::Json(
            serde_json::json!({
                "schema": "aethyme.broker-error/v1",
                "error": {
                    "code": code,
                    "message": message,
                },
            })
            .to_string(),
        )
    } else {
        ErrorRendering::Text(message.to_string())
    }
}

fn emit_cli_error(rendering: ErrorRendering) {
    match rendering {
        ErrorRendering::Json(json) => out!("{}", json),
        ErrorRendering::Text(message) => eprintln!("Error: {message}"),
        ErrorRendering::Diagnostic(message) => eprintln!("Error: {message}"),
    }
}

fn error_output_for_command(
    args: &[String],
    code: u8,
    message: &str,
    output_attempted: bool,
) -> (u8, ErrorRendering) {
    (
        code,
        render_cli_error(args, code, message, output_attempted),
    )
}

fn report_command_error(args: &[String], code: u8, message: &str, output_attempted: bool) -> u8 {
    let before = crate::cli_output::snapshot();
    let (code, rendering) = error_output_for_command(args, code, message, output_attempted);
    emit_cli_error(rendering);
    crate::cli_output::exit_code_after_output(before, code)
}

/// Render an error raised by router preflight before broker dispatch.
/// The JSON form is a standalone, versioned error envelope.
pub fn report_dispatch_error(args: &[String], code: u8, message: &str) -> u8 {
    report_command_error(args, code, message, false)
}

/// Keep the repository's primary checkout current before broker commands
/// inspect it. A linked session worktree is never moved here; it has the
/// explicit `broker sync --session <id>` path instead.
fn maybe_fast_forward_main_checkout(args: &[String], mode: &CompatibilityMode) {
    // A new session fetches its own upstream and allocates an independent
    // checkout. Scanning or moving the primary checkout is unrelated work,
    // can wait on its index, and can run expensive user Git hooks.
    let creates_new_session = match args.first().map(String::as_str) {
        // Parse flags rather than scanning raw tokens: task/path values may
        // themselves equal an option. Invalid requests need no maintenance.
        Some("start") => !parse(&args[1..])
            .is_ok_and(|parsed| parsed.adopt || parsed.reuse || parsed.replace_stale),
        Some("start-agent") => true,
        _ => false,
    };
    if creates_new_session || !matches!(mode, CompatibilityMode::Normal) || routine_status(args) {
        return;
    }
    // Only the everyday public commands keep the checkout current. Hooks run
    // inside a Git operation (a commit, a push) and must never move HEAD
    // under it, and advanced plumbing (`git`, `gh`, `gates`, …, resolved
    // here without its `advanced` prefix) acts on the checkout exactly as its
    // caller found it, within its own time budget.
    if !matches!(
        args.first().map(String::as_str),
        Some("status" | "start" | "sync" | "push" | "submit" | "finish")
    ) {
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
    let (Ok(checkout_root), Ok(main_root)) =
        (checkout.root().canonicalize(), main_root.canonicalize())
    else {
        return;
    };
    if checkout_root != main_root {
        return;
    }

    let status_command = format!(
        "git -C {} status --short --branch",
        crate::broker::shell_quote(&main_root.to_string_lossy())
    );
    let Ok(branch) = checkout.current_branch() else {
        report_main_checkout_unchanged("could not determine the current branch", &status_command);
        return;
    };
    if branch == "HEAD" {
        report_main_checkout_unchanged("the checkout is detached", &status_command);
        return;
    }
    let clean = match (
        checkout.tracked_dirty_paths(),
        checkout.untracked_paths_readonly(),
    ) {
        (Ok(tracked), Ok(untracked)) => tracked.is_empty() && untracked.is_empty(),
        _ => {
            report_main_checkout_unchanged(
                "could not verify that the checkout is clean",
                &status_command,
            );
            return;
        }
    };
    if !clean {
        report_main_checkout_unchanged(
            "tracked or non-ignored untracked changes are present",
            &status_command,
        );
        return;
    }
    match checkout.in_progress_operation() {
        Ok(None) => {}
        Ok(Some(operation)) => {
            report_main_checkout_unchanged(operation, &status_command);
            return;
        }
        Err(_) => {
            report_main_checkout_unchanged(
                "could not verify that no Git operation is in progress",
                &status_command,
            );
            return;
        }
    }
    // A session adopted on the primary checkout means an agent works in it:
    // moving HEAD under that agent is never this command's decision.
    match live_session_in_checkout(&main_root) {
        Ok(None) => {}
        Ok(Some(session)) => {
            report_main_checkout_unchanged(
                &format!("live broker session {session} works in this checkout"),
                &format!("aethyme broker status --session {session}"),
            );
            return;
        }
        Err(()) => {
            report_main_checkout_unchanged(
                "could not verify that no live broker session works in this checkout",
                "aethyme broker status",
            );
            return;
        }
    }
    // No upstream means nothing to fast-forward to; that is not a state that
    // needs attention on every command.
    let Some((upstream_ref, upstream_commit)) = checkout.tracking_upstream() else {
        return;
    };
    let Ok(head) = checkout.head_commit() else {
        report_main_checkout_unchanged("could not determine the checkout HEAD", &status_command);
        return;
    };
    if head == upstream_commit {
        return;
    }
    if checkout.is_ancestor(&upstream_commit, &head) {
        let range = format!("{upstream_ref}..HEAD");
        report_main_checkout_unchanged(
            "the checkout has commits that its upstream does not contain",
            &format!(
                "git -C {} log --oneline --decorate {}",
                crate::broker::shell_quote(&main_root.to_string_lossy()),
                crate::broker::shell_quote(&range)
            ),
        );
        return;
    }
    if !checkout.is_ancestor(&head, &upstream_commit) {
        let range = format!("HEAD...{upstream_ref}");
        report_main_checkout_unchanged(
            "the checkout and its upstream have diverged",
            &format!(
                "git -C {} log --oneline --decorate --left-right {}",
                crate::broker::shell_quote(&main_root.to_string_lossy()),
                crate::broker::shell_quote(&range)
            ),
        );
        return;
    }

    // Recheck the branch, refs, and working tree immediately before the
    // update. `--ff-only` remains Git's final guard against a changed graph.
    let still_same = checkout.current_branch().ok().as_deref() == Some(branch.as_str())
        && checkout.head_commit().ok().as_deref() == Some(head.as_str())
        && checkout
            .tracking_upstream()
            .is_some_and(|(current_ref, current_commit)| {
                current_ref == upstream_ref && current_commit == upstream_commit
            });
    let still_clean = matches!(
        (
            checkout.tracked_dirty_paths(),
            checkout.untracked_paths_readonly()
        ),
        (Ok(tracked), Ok(untracked)) if tracked.is_empty() && untracked.is_empty()
    );
    if !still_same || !still_clean {
        report_main_checkout_unchanged(
            "the checkout changed during fast-forward preflight",
            &status_command,
        );
        return;
    }
    if checkout
        .fast_forward_checkout_from(&branch, &head, &upstream_commit)
        .is_err()
    {
        report_main_checkout_unchanged(
            "Git could not safely fast-forward to the configured upstream (HEAD moved, local changes appeared, or the index was locked)",
            &format!(
                "git -C {} merge --ff-only {}",
                crate::broker::shell_quote(&main_root.to_string_lossy()),
                crate::broker::shell_quote(&upstream_commit)
            ),
        );
        return;
    }

    let trigger = safe_command_surface(args)
        .map(|surface| format!("broker.{surface}"))
        .unwrap_or_else(|| "broker.unknown".to_string());
    let payload = crate::events::broker_checkout_fast_forwarded_payload(
        &trigger,
        &branch,
        &upstream_ref,
        &head,
        &upstream_commit,
    );
    let recorded = crate::BrokerStore::open_in_repo(&main_root).and_then(|mut store| {
        store.append_event(
            crate::events::BROKER_CHECKOUT_FAST_FORWARDED,
            None,
            Some(&payload),
        )
    });
    crate::warn_unrecorded("record the main-checkout fast-forward event", recorded);
    eprintln!(
        "Updated main checkout: {branch} fast-forwarded from {head} to {upstream_commit} ({upstream_ref})."
    );
}

/// The first non-closed broker session whose worktree is this primary
/// checkout. `Err` when the broker database exists but cannot be read: the
/// caller then cannot rule out an agent working here and must not move it.
fn live_session_in_checkout(main_root: &std::path::Path) -> Result<Option<i64>, ()> {
    let Ok(db) = crate::broker_db_path(main_root) else {
        return Err(());
    };
    if !db.exists() {
        return Ok(None);
    }
    let store = crate::BrokerStore::open_snapshot_in_repo(main_root).map_err(|_| ())?;
    let sessions = store.live_sessions().map_err(|_| ())?;
    Ok(sessions
        .into_iter()
        .filter(|session| !session.status.is_closed())
        .find(|session| {
            let path = std::path::Path::new(&session.worktree_path);
            path == main_root
                || path
                    .canonicalize()
                    .is_ok_and(|canonical| canonical == main_root)
        })
        .map(|session| session.id))
}

fn report_main_checkout_unchanged(reason: &str, command: &str) {
    eprintln!("warning: left the main checkout unchanged: {reason}. Run: {command}");
}

#[derive(Debug)]
enum UsageError {
    Help,
    Message(String),
    Exit { message: String, code: u8 },
    SilentExit(u8),
}

impl<E: std::fmt::Display + 'static> From<E> for UsageError {
    fn from(err: E) -> Self {
        // `?` funnels every error through here, so this is the one place a
        // typed broker error can keep its class as an exit code.
        let code = (&err as &dyn std::any::Any)
            .downcast_ref::<crate::BrokerOpError>()
            .map_or(
                crate::exit_status::FAILED,
                crate::exit_status::for_broker_error,
            );
        if code == crate::exit_status::FAILED {
            UsageError::Message(err.to_string())
        } else {
            UsageError::Exit {
                message: err.to_string(),
                code,
            }
        }
    }
}

/// Cloned per pull request by `review tick`, which routes many of them from one
/// parse of one command line.
#[derive(Clone)]
struct Parsed {
    read_only_snapshot: bool,
    /// Why `read_only_snapshot` is set; refusals quote it.
    read_only_compatibility: Option<ReadOnlyCompatibility>,
    /// `submit --verify-only`: run verification and promote nothing.
    verify_only: bool,
    /// `status --summary`: skip the per-session lease refresh (#182).
    summary: bool,
    /// `status --compact`: emit a compact routine response.
    compact: bool,
    refresh: bool,
    positional: Vec<String>,
    task: Option<String>,
    cmd: Option<String>,
    target: Option<String>,
    repository: Option<String>,
    /// Explicit review target. Unlike `--pr`, this is only meaningful on
    /// `start` and `start-agent`, where it refuses an integration-tip lane.
    pull_request: Option<i64>,
    scope: Option<String>,
    effect: Option<String>,
    outcome: Option<String>,
    inspect_remote: bool,
    reason: Option<String>,
    agent: Option<String>,
    repo_name: Option<String>,
    tab_name: Option<String>,
    short_name: Option<String>,
    ai_provider: Option<String>,
    pr_number: Option<i64>,
    open_pr: bool,
    session: Option<i64>,
    to_session: Option<i64>,
    cross_session: Option<i64>,
    ref_write_acknowledged: bool,
    note_id: Option<i64>,
    /// Opaque durable guarded-exec receipt id.
    exec_receipt: Option<String>,
    message: Option<String>,
    entry: Option<i64>,
    confirm: Option<String>,
    broker_bin: Option<PathBuf>,
    delivery_mode: Option<String>,
    delivery_plan: Option<String>,
    operation: Option<String>,
    before: Option<i64>,
    limit: Option<u32>,
    detail: bool,
    active: bool,
    status: Option<String>,
    provider: Option<String>,
    adapter: Option<String>,
    worker: Option<String>,
    policy: Option<String>,
    error_code: Option<String>,
    watch_id: Option<i64>,
    /// `deliveries subscribe --repo-watch`: a repository watch id (#606).
    repo_watch_id: Option<i64>,
    /// `watch repo start` filters and options (#606).
    include_drafts: bool,
    include_existing: bool,
    exclude_authors: Option<String>,
    auto_watch: bool,
    generation: Option<i64>,
    events: Option<String>,
    ttl_seconds: Option<i64>,
    /// `leases wait --timeout`, `finish --timeout`, and `exec --timeout` values, in seconds.
    timeout_seconds: Option<u64>,
    wait: Option<String>,
    since: Option<i64>,
    kind: Option<String>,
    keep_days: Option<i64>,
    /// `insights --days`: reporting window in days. `0` means all history.
    days: Option<i64>,
    /// `insights --session-limit`: cap on per-session rows.
    session_limit: Option<i64>,
    /// `insights --pull-request-limit`: cap on per-pull-request rows.
    pull_request_limit: Option<i64>,
    seconds: Option<u64>,
    upstream: Option<String>,
    base: Option<String>,
    head: Option<String>,
    resolution_file: Option<PathBuf>,
    tabs_file: Option<PathBuf>,
    review_type: Option<String>,
    review_state: Option<String>,
    note: Option<String>,
    verdict: Option<String>,
    completed_for_commit: Option<String>,
    reviewer_provider: Option<String>,
    reviewer_model: Option<String>,
    /// Read the change from the provider rather than the working directory.
    ///
    /// `review run` normally describes the checkout it is standing in. A tick
    /// visits many pull requests and stands in none of them, so it asks the
    /// provider for the files and commit messages instead.
    from_provider: bool,
    write_resolution_template: Option<PathBuf>,
    worktree: Option<PathBuf>,
    title: Option<String>,
    /// `issues upsert --fingerprint`: the outcome key (#1238).
    fingerprint: Option<String>,
    /// `issues upsert --body-file`.
    body_file: Option<PathBuf>,
    /// `issues upsert --label`, repeatable.
    labels: Vec<String>,
    /// `issues upsert --resolve`.
    resolve: bool,
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
    /// `start --adopt`: register an existing worktree (formerly `adopt`).
    adopt: bool,
    replace_stale: bool,
    /// `--take-over`: move a session held by another live agent to this one.
    take_over: bool,
    all: bool,
    all_cleaned: bool,
    cleanup_batch: bool,
    allow_adopted: bool,
    retire: bool,
    archive: bool,
    abandon: bool,
    keep_worktree: bool,
    chau7: bool,
    fix_version: bool,
    with_gate: bool,
    apply: bool,
    dry_run: bool,
    include_active_gate_cache: bool,
    /// `gc plan`/`gc apply`: request exhaustive inventory instead of the
    /// default deadline-bound operator scan.
    full_scan: bool,
    /// `worktrees --measure`: walk every checkout instead of a budget.
    measure: bool,
    /// `gates affected --timings`: emit the timing object instead of the array.
    timings: bool,
    destructive: bool,
    no_wait: bool,
    allow_parallel: bool,
    queue_timeout_seconds: Option<u64>,
    break_glass: bool,
    sync_main: bool,
    sync_integration: bool,
    allow_main_checkout: bool,
    no_cache: bool,
    probe: bool,
    only: Option<String>,
    ship_only: bool,
    stdout: bool,
    include_task: bool,
    offline: bool,
    required_mode: Option<String>,
    planned_paths: Vec<String>,
    declared_scopes: Vec<String>,
    exec_command: Vec<String>,
    non_interactive: bool,
    /// `merge-chain` tuning: merge method, poll interval, timeouts, gates workflow.
    merge_chain: MergeChainFlags,
    /// Every flag spelled on the command line, in order, as written (`--` for
    /// the command separator). `validate_flags` checks these against the
    /// subcommand's entry in `FLAG_RULES`.
    given_flags: Vec<String>,
}

fn parse(args: &[String]) -> Result<Parsed, UsageError> {
    parse_with_context(args, false)
}

fn parse_with_context(args: &[String], ship_command: bool) -> Result<Parsed, UsageError> {
    let mut parsed = Parsed {
        read_only_snapshot: false,
        read_only_compatibility: None,
        verify_only: false,
        summary: false,
        compact: false,
        refresh: false,
        positional: Vec::new(),
        task: None,
        cmd: None,
        target: None,
        repository: None,
        pull_request: None,
        scope: None,
        effect: None,
        outcome: None,
        inspect_remote: false,
        reason: None,
        agent: None,
        repo_name: None,
        tab_name: None,
        short_name: None,
        ai_provider: None,
        pr_number: None,
        open_pr: false,
        session: None,
        to_session: None,
        cross_session: None,
        ref_write_acknowledged: false,
        note_id: None,
        exec_receipt: None,
        message: None,
        entry: None,
        confirm: None,
        broker_bin: None,
        delivery_mode: None,
        delivery_plan: None,
        operation: None,
        before: None,
        limit: None,
        detail: false,
        active: false,
        status: None,
        provider: None,
        adapter: None,
        worker: None,
        policy: None,
        error_code: None,
        watch_id: None,
        repo_watch_id: None,
        include_drafts: false,
        include_existing: false,
        exclude_authors: None,
        auto_watch: false,
        generation: None,
        events: None,
        ttl_seconds: None,
        timeout_seconds: None,
        wait: None,
        since: None,
        kind: None,
        keep_days: None,
        days: None,
        session_limit: None,
        pull_request_limit: None,
        seconds: None,
        upstream: None,
        base: None,
        head: None,
        resolution_file: None,
        tabs_file: None,
        review_type: None,
        review_state: None,
        note: None,
        verdict: None,
        completed_for_commit: None,
        reviewer_provider: None,
        reviewer_model: None,
        from_provider: false,
        write_resolution_template: None,
        worktree: None,
        title: None,
        fingerprint: None,
        body_file: None,
        labels: Vec::new(),
        resolve: false,
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
        adopt: false,
        replace_stale: false,
        take_over: false,
        all: false,
        all_cleaned: false,
        cleanup_batch: false,
        allow_adopted: false,
        retire: false,
        archive: false,
        abandon: false,
        keep_worktree: false,
        chau7: false,
        fix_version: false,
        with_gate: false,
        apply: false,
        dry_run: false,
        include_active_gate_cache: false,
        full_scan: false,
        measure: false,
        timings: false,
        destructive: false,
        no_wait: false,
        allow_parallel: false,
        queue_timeout_seconds: None,

        break_glass: false,
        sync_main: false,
        sync_integration: false,
        allow_main_checkout: false,
        no_cache: false,
        probe: false,
        only: None,
        ship_only: false,
        stdout: false,
        include_task: false,
        offline: false,
        required_mode: None,
        planned_paths: Vec::new(),
        declared_scopes: Vec::new(),
        exec_command: Vec::new(),
        non_interactive: false,
        merge_chain: MergeChainFlags::default(),
        given_flags: Vec::new(),
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg.starts_with('-') {
            parsed.given_flags.push(arg.clone());
        }
        match arg.as_str() {
            "--" => {
                parsed.exec_command = iter.cloned().collect();
                break;
            }
            "--json" | "--json-output" => parsed.json = true,
            "--non-interactive" => parsed.non_interactive = true,
            "--summary" => parsed.summary = true,
            "--compact" => parsed.compact = true,
            "--refresh" => parsed.refresh = true,
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
            "--detail" => parsed.detail = true,
            "--active" => parsed.active = true,
            "--force" => parsed.force = true,
            "--check" => parsed.check = true,
            "--dispatch" => parsed.dispatch = true,
            "--reuse" => parsed.reuse = true,
            "--adopt" => parsed.adopt = true,
            "--all" => parsed.all = true,
            "--all-cleaned" => parsed.all_cleaned = true,
            "--archive" => parsed.archive = true,
            "--batch" => parsed.cleanup_batch = true,
            "--allow-adopted" => parsed.allow_adopted = true,
            "--retire" => parsed.retire = true,
            "--abandon" => parsed.abandon = true,
            "--keep-worktree" => parsed.keep_worktree = true,
            "--open-pr" => parsed.open_pr = true,
            "--chau7" => parsed.chau7 = true,
            "--fix-version" => parsed.fix_version = true,
            "--with-gate" => parsed.with_gate = true,
            "--apply" => parsed.apply = true,
            "--dry-run" => parsed.dry_run = true,
            "--include-active-gate-cache" => parsed.include_active_gate_cache = true,
            "--full-scan" => parsed.full_scan = true,
            "--measure" => parsed.measure = true,
            "--timings" => parsed.timings = true,
            "--from-provider" => parsed.from_provider = true,
            "--no-wait" => parsed.no_wait = true,
            "--inspect-remote" => parsed.inspect_remote = true,
            "--queue-timeout" => {
                let value = iter.next().ok_or(UsageError::Message(
                    "--queue-timeout requires a value in seconds".into(),
                ))?;
                parsed.queue_timeout_seconds = Some(value.parse().map_err(|_| {
                    UsageError::Message(
                        "--queue-timeout must be an integer number of seconds".into(),
                    )
                })?);
            }
            "--destructive" => parsed.destructive = true,
            flag @ ("--merge-method" | "--poll-seconds" | "--checks-timeout" | "--main-timeout"
            | "--dispatch-after" | "--gates-workflow") => {
                let value = iter
                    .next()
                    .ok_or(UsageError::Message(format!("{flag} requires a value")))?;
                parsed.merge_chain.set(flag, value)?;
            }
            "--allow-parallel" => parsed.allow_parallel = true,
            "--break-glass" => parsed.break_glass = true,
            "--sync-main" => parsed.sync_main = true,
            "--sync-integration" => parsed.sync_integration = true,
            "--allow-main-checkout" => parsed.allow_main_checkout = true,
            "--no-cache" => parsed.no_cache = true,
            "--verify-only" => parsed.verify_only = true,
            "--probe" => parsed.probe = true,
            "--only" if ship_command => parsed.ship_only = true,
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
            "--include-drafts" => parsed.include_drafts = true,
            "--include-existing" => parsed.include_existing = true,
            "--auto-watch" => parsed.auto_watch = true,
            "--exclude-authors" => {
                parsed.exclude_authors = Some(
                    iter.next()
                        .ok_or(UsageError::Message(
                            "--exclude-authors requires a comma-separated list of logins".into(),
                        ))?
                        .clone(),
                );
            }
            "--repo-watch" => {
                let value = iter
                    .next()
                    .ok_or(UsageError::Message("--repo-watch requires a value".into()))?;
                parsed.repo_watch_id = Some(value.parse().map_err(|_| {
                    UsageError::Message(
                        "--repo-watch must be an integer repository watch id".into(),
                    )
                })?);
            }
            "--offline" => parsed.offline = true,
            "--require" => {
                parsed.required_mode = Some(
                    iter.next()
                        .ok_or(UsageError::Message(
                            "--require requires a readiness level".into(),
                        ))?
                        .clone(),
                )
            }
            "--replace-stale" => parsed.replace_stale = true,
            "--take-over" => parsed.take_over = true,
            "--path" => parsed.planned_paths.push(
                iter.next()
                    .ok_or(UsageError::Message(
                        "--path requires a repository-relative path".into(),
                    ))?
                    .clone(),
            ),
            // Repeatable: a session may name several targets up front.
            // `--scope` is already the coordinated-operation resource, so the
            // session's own targets are claimed, parallel to `leases claim`.
            "--claim" => parsed.declared_scopes.push(
                iter.next()
                    .ok_or(UsageError::Message(
                        "--claim requires kind:value[=operation], e.g. symbol:Name=replace".into(),
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
            "--days" => {
                // `0` is meaningful and not a typo: it asks for all recorded
                // history, which is expressed as having no window at all.
                let value = iter
                    .next()
                    .ok_or(UsageError::Message("--days requires a value".into()))?;
                parsed.days = Some(value.parse().map_err(|_| {
                    UsageError::Message("--days must be a non-negative integer".into())
                })?);
            }
            "--session-limit" => {
                let value = iter.next().ok_or(UsageError::Message(
                    "--session-limit requires a value".into(),
                ))?;
                parsed.session_limit = Some(value.parse().map_err(|_| {
                    UsageError::Message("--session-limit must be a non-negative integer".into())
                })?);
            }
            "--pull-request-limit" => {
                let value = iter.next().ok_or(UsageError::Message(
                    "--pull-request-limit requires a value".into(),
                ))?;
                parsed.pull_request_limit = Some(value.parse().map_err(|_| {
                    UsageError::Message(
                        "--pull-request-limit must be a non-negative integer".into(),
                    )
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
            "--tabs-file" => {
                parsed.tabs_file = Some(PathBuf::from(
                    iter.next()
                        .ok_or(UsageError::Message("--tabs-file requires a path".into()))?
                        .clone(),
                ));
            }
            "--type" => {
                parsed.review_type = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--type requires a review type".into()))?
                        .clone(),
                )
            }
            "--state" => {
                parsed.review_state = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--state requires a state".into()))?
                        .clone(),
                )
            }
            "--note" => {
                parsed.note = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--note requires text".into()))?
                        .clone(),
                )
            }
            "--verdict" => {
                parsed.verdict = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--verdict requires a value".into()))?
                        .clone(),
                )
            }
            "--completed-for-commit" => {
                parsed.completed_for_commit = Some(
                    iter.next()
                        .ok_or(UsageError::Message(
                            "--completed-for-commit requires a commit".into(),
                        ))?
                        .clone(),
                )
            }
            "--reviewer-provider" => {
                parsed.reviewer_provider = Some(
                    iter.next()
                        .ok_or(UsageError::Message(
                            "--reviewer-provider requires a provider".into(),
                        ))?
                        .clone(),
                )
            }
            "--reviewer-model" => {
                parsed.reviewer_model = Some(
                    iter.next()
                        .ok_or(UsageError::Message(
                            "--reviewer-model requires a model".into(),
                        ))?
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
            "--fingerprint" => {
                parsed.fingerprint = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--fingerprint requires a value".into()))?
                        .clone(),
                )
            }
            "--body-file" => {
                parsed.body_file = Some(PathBuf::from(
                    iter.next()
                        .ok_or(UsageError::Message("--body-file requires a path".into()))?
                        .clone(),
                ))
            }
            "--label" => parsed.labels.push(
                iter.next()
                    .ok_or(UsageError::Message("--label requires a value".into()))?
                    .clone(),
            ),
            "--resolve" => parsed.resolve = true,
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
            "--broker-bin" => {
                parsed.broker_bin = Some(PathBuf::from(iter.next().ok_or_else(|| {
                    UsageError::Message("--broker-bin requires an absolute router path".into())
                })?));
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
            "--repo-name" => {
                parsed.repo_name = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--repo-name requires a value".into()))?
                        .clone(),
                )
            }
            "--tab-name" => {
                parsed.tab_name = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--tab-name requires a value".into()))?
                        .clone(),
                )
            }
            "--short-name" => {
                parsed.short_name = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--short-name requires a value".into()))?
                        .clone(),
                )
            }
            "--ai-provider" => {
                parsed.ai_provider = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--ai-provider requires a value".into()))?
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
            "--pull-request" => {
                let value = iter.next().ok_or(UsageError::Message(
                    "--pull-request requires a positive integer".into(),
                ))?;
                let number = value.parse::<i64>().map_err(|_| {
                    UsageError::Message("--pull-request must be a positive integer".into())
                })?;
                if number <= 0 {
                    return Err(UsageError::Message(
                        "--pull-request must be a positive integer".into(),
                    ));
                }
                parsed.pull_request = Some(number);
            }
            "--session" => {
                let value = iter
                    .next()
                    .ok_or(UsageError::Message("--session requires a value".into()))?;
                parsed.session = Some(value.parse().map_err(|_| {
                    UsageError::Message("--session must be an integer session id".into())
                })?);
            }
            "--receipt" => {
                parsed.exec_receipt = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--receipt requires a value".into()))?
                        .clone(),
                );
            }
            "--ref-write-acknowledged" => parsed.ref_write_acknowledged = true,
            "--cross-session" => {
                let value = iter.next().ok_or(UsageError::Message(
                    "--cross-session requires a session id".into(),
                ))?;
                parsed.cross_session = Some(value.parse().map_err(|_| {
                    UsageError::Message("--cross-session must be an integer session id".into())
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
            "--delivery" => {
                parsed.delivery_mode = Some(
                    iter.next()
                        .ok_or(UsageError::Message("--delivery requires a value".into()))?
                        .clone(),
                )
            }
            "--plan" | "--plan-digest" => {
                parsed.delivery_plan = Some(
                    iter.next()
                        .ok_or(UsageError::Message(
                            "--plan requires a SHA-256 digest".into(),
                        ))?
                        .clone(),
                )
            }
            "--operation" => {
                let value = iter
                    .next()
                    .ok_or(UsageError::Message("--operation requires a value".into()))?;
                parsed.operation = Some(value.clone());
            }
            "--timeout" => {
                let value = iter
                    .next()
                    .ok_or(UsageError::Message("--timeout requires seconds".into()))?;
                parsed.timeout_seconds = Some(value.parse().map_err(|_| {
                    UsageError::Message("--timeout must be a whole number of seconds".into())
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

fn open_broker(read_only_snapshot: bool) -> Result<Broker, UsageError> {
    let cwd = std::env::current_dir()
        .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
    if read_only_snapshot {
        Ok(Broker::open_snapshot(&cwd)?)
    } else {
        Ok(Broker::open(&cwd)?)
    }
}

fn open_broker_for_explicit_worktree_root_recovery() -> Result<Broker, UsageError> {
    let cwd = std::env::current_dir()
        .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
    Ok(Broker::open_for_explicit_worktree_root_recovery(&cwd)?)
}

/// Starting or adopting a session is the one useful moment to surface a
/// stale installed broker. The check is advisory and writes to stderr so a
/// `--json` session report remains machine-readable.
fn warn_stale_broker_binary(broker: &Broker) {
    broker.warn_base_failure_incidents();
    if let Some(warning) = crate::version::broker_start_warning(broker.main_root()) {
        eprintln!("warning: {warning}");
    }
}

fn run_inner(args: &[String], mode: &CompatibilityMode) -> Result<(), UsageError> {
    let Some(subcommand) = args.first() else {
        return Err(UsageError::Help);
    };
    // Everywhere else `--pr` takes a pull-request number; for `push` it is a
    // switch. Translate it before parsing, and only give the parser ship
    // context because `--only` normally takes a value for other commands.
    let rest: Vec<String> = if subcommand == "push" {
        let separator = args
            .iter()
            .position(|arg| arg == "--")
            .unwrap_or(args.len());
        args[1..]
            .iter()
            .enumerate()
            .map(|(index, arg)| {
                if arg == "--pr" && index + 1 < separator {
                    "--open-pr".to_string()
                } else {
                    arg.clone()
                }
            })
            .collect()
    } else {
        args[1..].to_vec()
    };
    let parse_result = if subcommand == "ship" {
        parse_with_context(&rest, true)
    } else {
        parse(&rest)
    };
    let mut parsed = parse_result.map_err(|error| {
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
    if let CompatibilityMode::ReadOnlySnapshot(compatibility) = mode {
        parsed.read_only_snapshot = true;
        parsed.read_only_compatibility = Some(compatibility.clone());
    }
    if subcommand == "exec-status" {
        parsed.read_only_snapshot = true;
    }
    // `start` is the one public verb that merges by flag: registering an
    // existing worktree and spawning a process keep their own handlers.
    let subcommand = &match subcommand.as_str() {
        "start" if parsed.adopt || parsed.reuse || parsed.replace_stale => "adopt".to_string(),
        "start" if parsed.cmd.is_some() => "start-agent".to_string(),
        other => other.to_string(),
    };
    // `submit` has no positional subcommands. Refuse them before advisories or
    // holder admission can touch broker state; otherwise a typo such as
    // `submit status` falls through to the stateful submit handler.
    if subcommand == "submit"
        && let Some(unknown) = parsed.positional.first()
    {
        return Err(UsageError::Exit {
            message: surface::unsupported_submit_subcommand(unknown),
            code: crate::exit_status::USAGE,
        });
    }
    let exec_status_request = if subcommand == "exec-status" {
        Some(validated_exec_status_request(&parsed)?)
    } else {
        None
    };
    // One declarative check replaces the per-flag guards that grew after #285:
    // a flag the subcommand never reads is refused rather than dropped.
    validate_flags(subcommand, &parsed)?;
    if subcommand == "commit" {
        if parsed.read_only_snapshot {
            return Err(read_only_refusal(
                &parsed,
                "broker commit",
                "run it from the live session worktree",
            ));
        }
        if let Some(reason) = crate::git::session_commit_git_environment_refusal() {
            return Err(UsageError::Message(reason.into()));
        }
        crate::broker::validate_session_commit_args(&parsed.exec_command)
            .map_err(UsageError::Message)?;
    }
    let reclaim_action = if subcommand == "reclaim" {
        Some(parse_reclaim_action(&parsed)?)
    } else {
        None
    };
    // Routine status is an observation, including advisory delivery. Do not
    // migrate/create storage or mark notes delivered merely by polling it.
    if routine_status(args) {
        parsed.read_only_snapshot = true;
    }
    let task_lease_command = task_lease_command_class(subcommand, &parsed);
    if task_lease_command == TaskLeaseCommandClass::ObservationOnly {
        parsed.read_only_snapshot = true;
    }
    // Resolve through a non-mutating snapshot first, then acquire the exact
    // session guard before advisory delivery, holder admission, or a handler
    // can open mutable broker state and run maintenance.
    let session_operation = if task_lease_command
        == TaskLeaseCommandClass::ClosedSessionAdministration
    {
        None
    } else {
        acquire_session_operation_context(
            &parsed,
            subcommand == "finish" || closed_session_command_reaches_handler(subcommand, &parsed),
        )?
    };
    surface_command_advisories(subcommand, &parsed);
    // A closed `finish` is an idempotent recovery path, not new work for the
    // session. The guarded snapshot above already established that the row is
    // closed, and its shared operation guard stays held through the handler.
    // Holder admission still applies to open sessions and every other command.
    let closed_finish_retry = subcommand == "finish"
        && session_operation
            .as_ref()
            .is_some_and(|context| !context.session_open);
    if !closed_finish_retry {
        admit_session_holder(subcommand, &parsed)?;
    }
    let heartbeat = if subcommand == "heartbeat" {
        Some(heartbeat_session_task_lease(session_operation.as_ref())?)
    } else if session_operation
        .as_ref()
        .is_some_and(|context| context.session_open)
        && task_lease_command == TaskLeaseCommandClass::GuardedWork
    {
        if crate::file_lock::session_operation_guard_supported() {
            heartbeat_session_task_lease(session_operation.as_ref())?;
        }
        None
    } else if session_operation
        .as_ref()
        .is_some_and(|context| context.session_open)
        && task_lease_command == TaskLeaseCommandClass::OtherWork
    {
        downgrade_unsupported_session_operation(session_operation.as_ref())?;
        None
    } else {
        None
    };

    match subcommand.as_str() {
        "readiness" => run_readiness(parsed)?,
        "worktree-root" => run_worktree_root(parsed)?,
        "adopt" => run_adopt(parsed)?,
        "start" => run_start(parsed)?,
        "start-agent" => run_start_agent(parsed)?,
        "report" => run_report(parsed)?,
        "quality-report" => run_quality_report(parsed)?,
        "issues" => run_issues(parsed)?,
        "external-events" => run_external_events(parsed)?,
        "reclaim" => {
            let action = reclaim_action.ok_or_else(|| {
                UsageError::Message("reclaim action validation was skipped".into())
            })?;
            run_reclaim(parsed, action)?;
        }
        "deliveries" => run_deliveries(parsed)?,
        "review" => run_review(parsed)?,
        "review-thread" => run_review_thread(parsed)?,
        "prepare" => run_prepare(parsed)?,
        "console" => run_console(parsed)?,
        "resources" => run_resources(parsed)?,
        "agents" => run_agents(parsed)?,
        "leases" => run_leases(parsed)?,
        "ownership" => run_ownership(parsed)?,
        "exec" => run_exec(parsed)?,
        "commit" => run_commit(parsed)?,
        "exec-status" => {
            let request = exec_status_request.ok_or_else(|| {
                UsageError::Message("exec-status request validation was skipped".into())
            })?;
            run_exec_status(parsed.json, request)?;
        }
        "git" | "gh" => run_git_gh(parsed, subcommand)?,
        "merge-chain" => run_merge_chain(parsed)?,
        "advisories" => run_advisories(parsed)?,
        "exposures" => run_exposures(parsed)?,
        "note" => run_note(parsed)?,
        "operations" => run_operations(parsed)?,
        "gates" => run_gates(parsed)?,
        "watch" => run_pull_request_watch(parsed)?,
        "pr" => run_pr(parsed)?,
        "hooks" => run_hooks(parsed)?,
        "submit" => run_submit(parsed)?,
        "push" => run_push(parsed)?,
        "sync" => run_sync(parsed)?,
        "repair" => run_repair(parsed)?,
        "representation" => run_representation(parsed)?,
        "main" => run_main_reconcile(parsed)?,
        "promotion-record" => run_promotion_record(parsed)?,
        "checkpoint" => run_checkpoint(parsed)?,
        "queue" => run_queue(parsed)?,
        "promote" => run_promote(parsed)?,
        "ship" => run_ship(parsed)?,
        "integration" => run_integration(parsed)?,
        "status" => run_status(parsed)?,
        "events" => run_events(parsed)?,
        "metrics" => run_metrics(parsed)?,
        "insights" => run_insights(parsed)?,
        "blockers" => run_blockers(parsed)?,
        "unblock" => run_unblock(parsed)?,
        "doctor" => run_doctor(parsed)?,
        "quick-test" => run_quick_test(parsed)?,
        "trust" => run_trust_command(&parsed)?,
        "verify-loop" | "e2e" => run_verify_loop(parsed)?,
        "init" => run_init(parsed)?,
        "certify" | "scaffold" => run_certify_scaffold(parsed, subcommand)?,
        "handoff" => run_handoff(parsed)?,
        "heartbeat" => run_heartbeat(
            parsed,
            heartbeat.ok_or_else(|| {
                UsageError::Message("session heartbeat requires an acquired operation guard".into())
            })?,
        )?,
        "finish" => run_finish(parsed)?,
        "close" => run_close(parsed)?,
        "gc" => run_gc(parsed)?,
        "worktrees" => run_worktrees(parsed)?,
        "storage" => run_storage(parsed)?,
        "cleanup" => run_cleanup(parsed)?,
        "-h" | "--help" => return Err(UsageError::Help),
        other => {
            return Err(UsageError::Message(format!(
                "unknown broker subcommand {other:?} — see `aethyme broker --help`"
            )));
        }
    }
    Ok(())
}

/// Commands that act for a session, and so must come from its holder (#393).
const HOLDER_GUARDED: &[&str] = &[
    "submit",
    "push",
    "sync",
    "finish",
    "close",
    "git",
    "gh",
    "commit",
    "review-thread",
    "heartbeat",
];

struct SessionOperationContext {
    main_root: std::path::PathBuf,
    session_id: i64,
    session_open: bool,
    _guard: crate::file_lock::SessionOperationGuard,
}

/// Acquire a shared per-session guard before any mutating broker open can run
/// maintenance. Explicit IDs are checked against a read-only snapshot. With
/// no ID, only the exact current registered worktree is eligible; no arbitrary
/// path or parent-directory inference is used. `finish` may inspect a closed
/// row under the same guard to preserve its idempotent recovery path; new work
/// remains refused for every other command.
fn acquire_session_operation_context(
    parsed: &Parsed,
    allow_closed: bool,
) -> Result<Option<SessionOperationContext>, UsageError> {
    if parsed.read_only_snapshot {
        return Ok(None);
    }
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(_) if parsed.session.is_none() => return Ok(None),
        Err(error) => {
            return Err(UsageError::Message(format!(
                "cannot resolve the current checkout for session guard: {error}"
            )));
        }
    };
    let checkout = match crate::GitRepo::discover(&cwd) {
        Ok(checkout) => checkout,
        Err(_) if parsed.session.is_none() => return Ok(None),
        Err(error) => {
            return Err(UsageError::Message(format!(
                "cannot resolve the repository for session guard: {error}"
            )));
        }
    };
    let main_root = match checkout.main_root() {
        Ok(main_root) => main_root,
        Err(_) if parsed.session.is_none() => return Ok(None),
        Err(error) => {
            return Err(UsageError::Message(format!(
                "cannot resolve the primary repository for session guard: {error}"
            )));
        }
    };
    if parsed.session.is_none() && checkout.root() == main_root {
        return Ok(None);
    }
    if !main_root.join(crate::BROKER_DB_RELPATH).is_file() {
        return Ok(None);
    }
    let store = crate::BrokerStore::open_snapshot_in_repo(&main_root)?;
    let session_id = match parsed.session {
        Some(session_id) => session_id,
        None => match store.session_for_worktree(checkout.root().to_string_lossy().as_ref())? {
            Some(session) => session.id,
            None => return Ok(None),
        },
    };
    match store.session(session_id) {
        Ok(_) => {}
        Err(crate::BrokerError::SessionNotFound(_)) => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let guard = crate::file_lock::try_acquire_session_operation_guard(
        &main_root,
        session_id,
        crate::file_lock::SessionOperationLockMode::Shared,
    )?
    .ok_or_else(|| {
        UsageError::Message(format!(
            "session {session_id} is being retired; retry this command after maintenance finishes"
        ))
    })?;
    // Retirement may win after the read-only lookup but before the shared
    // guard is acquired. Revalidate the row while holding the guard so a
    // stale explicit ID cannot start work on a session that has already
    // crossed its retirement CAS.
    let current_store = crate::BrokerStore::open_snapshot_in_repo(&main_root)?;
    let current = current_store
        .session(session_id)
        .map_err(|error| match error {
            crate::BrokerError::SessionNotFound(_) => UsageError::Message(format!(
                "session {session_id} was retired before this command could start"
            )),
            other => other.into(),
        })?;
    let session_open =
        current.cleanup_state == crate::SessionCleanupState::Open && !current.status.is_closed();
    if !session_open && !allow_closed {
        return Err(UsageError::Message(format!(
            "session {session_id} is already closed; it cannot start new session work"
        )));
    }
    if parsed.session.is_none() {
        let checkout_root = checkout.root().to_string_lossy().into_owned();
        let current_binding = current_store.session_for_worktree(&checkout_root)?;
        if current_binding.as_ref().map(|session| session.id) != Some(session_id)
            || current.worktree_path != checkout_root
        {
            return Err(UsageError::Message(
                "the current checkout stopped resolving to the same open broker session while acquiring its operation guard".into(),
            ));
        }
    }
    Ok(Some(SessionOperationContext {
        main_root,
        session_id,
        session_open,
        _guard: guard,
    }))
}

/// Commands with an existing session ownership or active-work protocol renew
/// the holder lease. Read-only polling stays observation-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskLeaseCommandClass {
    /// Read-only inspection must not extend the lease or downgrade it.
    ObservationOnly,
    /// Existing command protocols leave durable witnesses for child use.
    GuardedWork,
    /// Known session work without a child-lifetime witness drops finite
    /// expiry authority before it starts.
    OtherWork,
    /// Administrative cleanup acts on an already-closed session and must not
    /// be rejected by the open-session admission fence or renew its task lease.
    ClosedSessionAdministration,
    /// Invalid command names never reach a handler and cannot use a session.
    NoSessionWork,
}

fn task_lease_command_class(subcommand: &str, parsed: &Parsed) -> TaskLeaseCommandClass {
    let action = parsed.positional.first().map(String::as_str);
    let nested_action = parsed.positional.get(1).map(String::as_str);
    match subcommand {
        "prepare" if action == Some("status") => TaskLeaseCommandClass::ObservationOnly,
        "leases" if matches!(action, None | Some("plan" | "explain" | "export")) => {
            TaskLeaseCommandClass::ObservationOnly
        }
        "note" if action == Some("list") => TaskLeaseCommandClass::ObservationOnly,
        "ownership" if matches!(action, None | Some("list")) => {
            TaskLeaseCommandClass::ObservationOnly
        }
        "hooks" if matches!(action, Some("status" | "snippet")) => {
            TaskLeaseCommandClass::ObservationOnly
        }
        "review" if action == Some("show") => TaskLeaseCommandClass::ObservationOnly,
        "representation" if matches!(action, None | Some("scan" | "status")) => {
            TaskLeaseCommandClass::ObservationOnly
        }
        "cleanup" if action == Some("audit") || (parsed.all_cleaned && !parsed.apply) => {
            TaskLeaseCommandClass::ObservationOnly
        }
        "cleanup" => TaskLeaseCommandClass::ClosedSessionAdministration,
        "operations" if matches!(action, None | Some("list" | "show")) => {
            TaskLeaseCommandClass::ObservationOnly
        }
        "operations" if action == Some("stats") => TaskLeaseCommandClass::ObservationOnly,
        "gates" if matches!(action, Some("status" | "manifest" | "scope")) => {
            TaskLeaseCommandClass::ObservationOnly
        }
        "gates" if action == Some("incidents") && nested_action == Some("list") => {
            TaskLeaseCommandClass::ObservationOnly
        }
        "events" if action != Some("prune") => TaskLeaseCommandClass::ObservationOnly,
        "external-events" if matches!(action, Some("list" | "show")) => {
            TaskLeaseCommandClass::ObservationOnly
        }
        "report"
            if matches!(action, Some("list" | "show"))
                || (action == Some("render") && parsed.output.is_none()) =>
        {
            TaskLeaseCommandClass::ObservationOnly
        }
        "quality-report" if action == Some("plan") => TaskLeaseCommandClass::ObservationOnly,
        "deliveries" if action == Some("list") => TaskLeaseCommandClass::ObservationOnly,
        "watch"
            if (matches!(action, Some("pr" | "repo"))
                && matches!(nested_action, Some("list" | "show" | "events" | "batches")))
                || (action == Some("pr")
                    && nested_action == Some("monitoring")
                    && parsed.positional.get(2).map(String::as_str) == Some("status"))
                || (action == Some("pr")
                    && nested_action == Some("service")
                    && matches!(
                        parsed.positional.get(2).map(String::as_str),
                        Some("plan" | "status")
                    )) =>
        {
            TaskLeaseCommandClass::ObservationOnly
        }
        "handoff" | "metrics" | "insights" | "blockers" | "exec-status" | "queue" | "worktrees" => {
            TaskLeaseCommandClass::ObservationOnly
        }
        "reclaim" if action == Some("plan") => TaskLeaseCommandClass::ObservationOnly,
        "reclaim" => TaskLeaseCommandClass::OtherWork,
        "gc" if action == Some("plan") => TaskLeaseCommandClass::ObservationOnly,
        "heartbeat" | "commit" | "exec" | "gh" | "git" | "prepare" | "push" | "submit" | "sync" => {
            TaskLeaseCommandClass::GuardedWork
        }
        "gates" if matches!(action, Some("run" | "pre-push")) => TaskLeaseCommandClass::GuardedWork,
        "resources" if action == Some("artifact-run") => TaskLeaseCommandClass::GuardedWork,
        "leases"
            if matches!(
                action,
                Some("claim" | "release" | "request-release" | "ack" | "decline" | "wait")
            ) =>
        {
            TaskLeaseCommandClass::GuardedWork
        }
        "note" if matches!(action, Some("send" | "ack")) => TaskLeaseCommandClass::GuardedWork,
        "ownership" if matches!(action, Some("claim" | "release")) => {
            TaskLeaseCommandClass::GuardedWork
        }
        "status" | "readiness" | "worktree-root" | "adopt" | "start" | "start-agent" | "report"
        | "quality-report" | "external-events" | "deliveries" | "review" | "review-thread"
        | "console" | "resources" | "agents" | "ownership" | "merge-chain" | "advisories"
        | "exposures" | "note" | "operations" | "gates" | "watch" | "pr" | "hooks" | "finish"
        | "close" | "gc" | "storage" | "unblock" | "doctor" | "quick-test" | "trust"
        | "verify-loop" | "e2e" | "init" | "certify" | "scaffold" | "promotion-record"
        | "checkpoint" | "promote" | "ship" | "integration" | "events" => {
            TaskLeaseCommandClass::OtherWork
        }
        _ => TaskLeaseCommandClass::NoSessionWork,
    }
}

/// These operations need to reach their domain handler for the established
/// closed-session refusal or lifecycle action. They still hold the shared
/// operation guard, and open sessions follow the normal heartbeat/downgrade
/// path below.
fn closed_session_command_reaches_handler(subcommand: &str, parsed: &Parsed) -> bool {
    matches!(
        (subcommand, parsed.positional.first().map(String::as_str)),
        ("leases", Some("claim"))
            | ("review", Some("request" | "unlock" | "reassign"))
            | ("reclaim", Some("apply"))
    )
}

fn heartbeat_session_task_lease(
    context: Option<&SessionOperationContext>,
) -> Result<crate::session_holder::TaskLeaseHeartbeat, UsageError> {
    let context = context.ok_or_else(|| {
        UsageError::Message("heartbeat requires a registered session in this repository".into())
    })?;
    if !crate::file_lock::session_operation_guard_supported() {
        return Ok(crate::session_holder::TaskLeaseHeartbeat::LegacyProtected);
    }
    let crate::session_holder::Caller::Agent(caller) = crate::session_holder::caller() else {
        return Ok(crate::session_holder::TaskLeaseHeartbeat::NotHolder);
    };
    let mut store = crate::BrokerStore::open_in_repo(&context.main_root)?;
    let latest = store.latest_session_holder_event(context.session_id)?;
    if latest
        .as_ref()
        .is_some_and(crate::session_holder::task_lease_binding_present)
    {
        let bound_holder = latest
            .as_ref()
            .and_then(|event| event.payload_json.as_deref())
            .and_then(|payload| {
                serde_json::from_str::<crate::session_holder::AgentProcess>(payload).ok()
            });
        if bound_holder.as_ref() != Some(&caller) {
            return Err(UsageError::Message(format!(
                "session {} has a task lease bound to a different or unreadable holder; run the command as that holder or adopt the session first",
                context.session_id
            )));
        }
    }
    store
        .heartbeat_session_task_lease(context.session_id, &caller, crate::clock::epoch_ms())
        .map_err(UsageError::from)
}

/// Before an opted-in session starts a command outside the known durable-child
/// protocols, remove its finite lease while the shared session guard is held.
/// This keeps the existing live-process protection authoritative for that
/// session until a guarded command explicitly upgrades it again.
fn downgrade_unsupported_session_operation(
    context: Option<&SessionOperationContext>,
) -> Result<(), UsageError> {
    let Some(context) = context else {
        return Ok(());
    };
    let caller = match crate::session_holder::caller() {
        crate::session_holder::Caller::Agent(caller) => Some(caller),
        crate::session_holder::Caller::Unidentified => None,
    };
    let mut store = crate::BrokerStore::open_in_repo(&context.main_root)?;
    match store.downgrade_session_task_lease_for_holder(
        context.session_id,
        caller.as_ref(),
        crate::clock::epoch_ms(),
    )? {
        crate::session_holder::TaskLeaseDowngrade::NotFinite
        | crate::session_holder::TaskLeaseDowngrade::Downgraded => Ok(()),
        crate::session_holder::TaskLeaseDowngrade::NotHolder => Err(UsageError::Message(format!(
            "session {} has a finite task lease; this command is not covered by its child-lifetime protocol and must be run by the current holder or after takeover",
            context.session_id
        ))),
    }
}

/// Refuse a command for a session that another live agent process holds,
/// unless `--take-over`. Anything this check cannot establish -- no broker
/// database, no such session, no `ps`, an unidentified caller -- lets the
/// command through to fail or succeed on its own terms.
fn admit_session_holder(subcommand: &str, parsed: &Parsed) -> Result<(), UsageError> {
    if parsed.read_only_snapshot || !HOLDER_GUARDED.contains(&subcommand) {
        return Ok(());
    }
    let Ok(cwd) = std::env::current_dir() else {
        return Ok(());
    };
    let Ok(checkout) = crate::GitRepo::discover(&cwd) else {
        return Ok(());
    };
    let Ok(main_root) = checkout.main_root() else {
        return Ok(());
    };
    if !main_root.join(crate::BROKER_DB_RELPATH).is_file() {
        return Ok(());
    }
    let mut store = crate::BrokerStore::open_in_repo(&main_root)?;
    let session_id = match parsed.session {
        Some(id) => id,
        None => match store.session_for_worktree(checkout.root().to_string_lossy().as_ref())? {
            Some(session) => session.id,
            None => return Ok(()),
        },
    };
    if store.session(session_id).is_err() {
        return Ok(());
    }
    admit_caller(&mut store, session_id, parsed.take_over)
}

/// [`admit_session_holder`] for a resolved session, reporting a transfer.
fn admit_caller(
    store: &mut crate::BrokerStore,
    session_id: i64,
    take_over: bool,
) -> Result<(), UsageError> {
    use crate::session_holder::{HolderCheck, ProcessTable, caller_in, reason};
    let Some(table) = ProcessTable::snapshot() else {
        return Ok(());
    };
    let caller = caller_in(
        &table,
        std::env::var(crate::session_holder::AGENT_PID_ENV)
            .ok()
            .as_deref(),
    );
    match crate::session_holder::admit(store, session_id, &caller, &table, take_over)? {
        HolderCheck::Bound {
            reason: why @ (reason::TAKE_OVER | reason::HOLDER_GONE),
        } => {
            eprintln!(
                "note: session {session_id} is now held by this agent ({})",
                if why == reason::TAKE_OVER {
                    "taken over from a live holder"
                } else {
                    "its previous holder is no longer running"
                }
            );
        }
        HolderCheck::UnknownBinding => {
            return Err(UsageError::Message(format!(
                "session {session_id} has unreadable holder metadata; refusing to replace its process-lifetime protection"
            )));
        }
        HolderCheck::Held | HolderCheck::Bound { .. } | HolderCheck::Unidentified => {}
    }
    Ok(())
}

/// Record the calling agent as the holder of a session it just started or
/// adopted, and the installed build that ran it. Best effort: the session
/// exists either way.
fn bind_caller_as_holder(store: &mut crate::BrokerStore, session_id: i64) {
    let caller = crate::session_holder::caller();
    if let Err(error) = crate::session_holder::bind(
        store,
        session_id,
        &caller,
        crate::session_holder::reason::REGISTERED,
    ) {
        eprintln!("warning: could not record session {session_id}'s holder: {error}");
    }
    // #293: the build that started or adopted the session, so a later swap
    // of the installed pair is reported instead of passing silently.
    crate::install_replacement::record(store, session_id);
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
    if matches!(subcommand, "hooks" | "readiness" | "storage") {
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
        crate::warn_unrecorded(
            "record advisory delivery",
            store.record_advisories_shown(&advisories, crate::AdvisoryDeliverySurface::Command),
        );
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
            "  acknowledge: aethyme broker advanced note ack --session {} --id {}",
            session_id, note.id
        );
    }
}
