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

mod flags;
mod gates;
mod gc_storage;
mod git_gh;
mod leases;
mod render;
mod report;
mod resources;
mod review;
mod session;
mod ship;
mod status;
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
use leases::*;
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
    ADVANCED_VERBS, DEPRECATED_SPELLING_REMOVAL_RELEASE, Deprecation, PUBLIC_VERBS, Resolution,
    deprecation_warning, help_text, resolve, wants_help,
};

const RESOURCES_RECONCILE_USAGE: &str =
    "usage: aethyme broker advanced resources reconcile <lease-id> --confirm <generation> [--json]";
const OPERATIONS_RECONCILE_USAGE: &str = "usage: aethyme broker advanced operations reconcile \
     --operation <id> --outcome <succeeded|failed> --reason <text> [--json]";
const OPERATIONS_SHOW_USAGE: &str = "usage: aethyme broker advanced operations show <id> [--json]";
const OPERATIONS_STATS_USAGE: &str = "usage: aethyme broker advanced operations stats [--repo <canonical-id>] [--limit <n>] [--json]";
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
  aethyme broker adopt [<path>] [--task <text>] [--path <repo-path>]... [--agent <name-and-email>] [--repo-name <name>] [--tab-name <name>] [--ai-provider <provider>] [--reuse [--sync-integration]|--replace-stale] [--json]
      Register an existing worktree (attach-first). Defaults to the
      current directory. If the worktree already has a session:
      --reuse points it at a follow-up task with a fresh baseline and
      reports its relation to the current integration tip;
      --sync-integration requires --reuse and first fast-forwards a clean
      session worktree to the exact integration tip;
      --replace-stale closes it and registers fresh; policy may reclaim known ignored build artifacts;
      neither flag = error listing your options. Every --path is validated
      and claimed explicitly in the same transaction as create/reuse.
  aethyme broker close --session <id> [--json]
      Low-level close. Retains the checkout and branch; policy may reclaim
      known ignored build artifacts. Does not check whether commits were
      submitted. Prefer finish for normal lifecycle use.
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
  aethyme broker quality-report plan <path> --repo <owner/name> --pr <number> [--json]
  aethyme broker quality-report publish <path> --repo <owner/name> --pr <number> --session <id> [--json]
      Validate and idempotently publish a revision-bound, redacted quality
      report as the broker-owned neutral/advisory PR summary. Publication is
      opt-in and uses the coordinated GitHub operation lane; it never changes
      gate selection or CI certification.
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
  aethyme broker start --task <text> [--base <ref>] [--pull-request <number>] [--path <repo-path>]... [--agent <name-and-email>] [--repo-name <name>] [--tab-name <name>] [--ai-provider <provider>] [--json]
      Create a broker-managed worktree + branch and register a session,
      atomically claiming every reviewed --path, but do not spawn a process.
      Prefer this over adopting the main
      checkout for agent work; it isolates the git index and worktree.
      --agent as in adopt (see above).
  aethyme broker start-agent --task <text> --cmd <command> [--pull-request <number>] [--agent <identity>] [--repo-name <name>] [--tab-name <name>] [--ai-provider <provider>] [--json]
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
  aethyme broker resources explain <request.json> [--json]
      Read-only diagnosis joining each blocked resource to its exact lease,
      holder process, liveness, and wait/reconcile guidance.
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
  aethyme broker resources reap [--json]
      Reclaim capacity from quarantined leases whose holder process is gone;
      named allocations remain quarantined for cleanup review.
  aethyme broker resources reconcile <lease-id> --confirm <generation> [--json]
      Release an expired, quarantined allocation after reviewing host cleanup.
      The generation confirmation fences stale cleanup commands.
  aethyme broker console [status|list] [--json]
      Show the exact revision of this checkout and list consoles serving this
      repository right now, including each marker's branch, commit, worktree,
      port, and integration relation. Read-only; reserves nothing.
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
  aethyme broker exec --session <id> -- <command> [--json]
      Run a command in the session worktree, then fail if it creates or
      modifies dirty paths outside explicit leases or in adoption-time
      foreign files. Exports AETHYME_TEST_DB_SUFFIX=s<id>-exec.
  aethyme broker git --session <id> [--repo <owner/name>] [--scope <scope>] [--effect <read|write|destructive>] [--reason <text>] [--destructive] [--no-wait|--queue-timeout <seconds>] [--json] -- <git-args>
      Run Git through the durable operation coordinator. Remote Git commands
      require an exact --repo. Repository writes are serialized, journaled,
      and fail closed after a crash with an unknown remote outcome.
      A write queues for the repository lock and is recorded while it waits, so
      `operations list` shows it. Use --no-wait to refuse rather than queue, or
      --queue-timeout <seconds> to give up after a bounded wait.
  aethyme broker gh --session <id> --repo <owner/name> [--scope <scope>] [--effect <read|write|destructive>] [--reason <text>] [--destructive] [--no-wait|--queue-timeout <seconds>] [--json] -- <gh-args>
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
  aethyme broker watch pr batches --id <watch-id> [--all] [--json]
  aethyme broker watch pr ack --id <batch-id> --outcome <addressed|stale|non-actionable|superseded> --reason <text> [--json]
      Persist a metadata-only PR watch and inspect it with one-shot polling.
      Aethyme records provider ids, authors, states, URLs and timestamps, but
      never comment/review bodies. `tick` performs one bounded foreground
      scheduling pass; the broker never starts a background poller.
  aethyme broker deliveries subscribe --watch <id> --adapter <name> --target <opaque-id> [--policy <notify|resume|review-and-push>] [--json]
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
  aethyme broker ship plan --entry <id> [--delivery <local_main_merge|pull_request>] [--json]
      Read-only delivery plan through an exact promoted entry: resolve the
      trusted delivery policy, divergence recommendation, publication prefix,
      remote freshness, proposed route, and local-main safety.
  aethyme broker ship execute --entry <id> --confirm <full-publication-sha> [--delivery <local_main_merge|pull_request>] [--plan <sha256>] [--sync-main] [--break-glass --reason <authorization>] [--json]
      Revalidate and execute the reviewed delivery route. Local-main delivery
      uses a clean fast-forward merge; pull-request delivery pushes a
      deterministic branch and opens or verifies one exact GitHub PR.
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
  aethyme broker status [--json] [--summary]
      The whole picture: agents, overlaps, promoted conflicts, merge
      queue, integration head. Session records are reported under the
      `agents` key; the id every `--session <id>` flag expects is
      `agents[].id`. There is no `sessions` key.
      --summary prints only `summary` and `advice`, and skips the
      per-session diff that dominates the full view's cost. Prefer it for
      the orientation step at the start of a session: it is cheap, and
      small enough that truncating it does not yield unparseable JSON.
      Its `overlap_count` is as of the last refresh, which it reports as
      `leases_refreshed: false`; use the full view when lease truth must
      be current.
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
  aethyme broker main reconcile plan [--detail] [--resolution-file <path>] [--write-resolution-template <path>] [--json]
      Read-only classification of everything the local default branch carries
      that the integration branch does not. A commit counts as already
      represented when integration holds its content for every path it touched,
      which recognizes work that landed through a squashed promotion and whose
      SHA therefore differs. Uncommitted tracked changes, or any commit that
      cannot be proven represented, refuse the apply.
  aethyme broker main reconcile apply --session <id> --confirm <sha256> [--resolution-file <path>] [--json]
      Move the local default branch onto integration after re-proving the
      reviewed plan. Creates a preservation ref at the pre-move tip first, and
      never runs when anything would be lost.
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
  aethyme broker reclaim plan [--json]
  aethyme broker reclaim apply --confirm <sha256> [--json]
      Report regenerable build output (target, node_modules, .venv, build,
      dist) in session worktrees, and remove only what was reviewed. An active
      session's artefacts are listed but never removed. Nothing here is
      recreated for you: a reclaimed worktree pays a cold build next time.
  aethyme broker gc plan [--include-active-gate-cache] [--json]
      Report exact retention-eligible rows, runtime files, represented
      worktrees/refs, finished sessions' build caches, this repository's gate
      cache, estimated bytes, blockers, and a stable plan digest. The newest
      gate cache of each kind is kept (active) unless
      --include-active-gate-cache is given; the next gate then rebuilds it
      from scratch.
  aethyme broker gc apply --confirm <sha256> [--include-active-gate-cache] [--json]
      Apply or resume the exact reviewed plan under an exclusive lock. A
      recovery journal makes interrupted row, file, and worktree cleanup safe.
      Pass --include-active-gate-cache when the plan was made with it.
  aethyme broker storage [--json]
  aethyme broker storage plan [--json]
  aethyme broker storage apply --confirm <sha256> [--json]
      Inventory every host worktree root by reconciling disk directories,
      Git registrations, and repository session ledgers. Apply removes only
      exact reviewed orphan roots or stray directories.
  aethyme broker check-contract [--base <ref>] [--pr-body <file>]
      Cross-process contract gate: refuse a diff that removes symbols
      listed in the consumers registry unless the PR body or commit
      messages declare a contract decision. Run by CI and by the
      `cross-process-contract` gate. Exit 1 = undeclared contract change.

Overlaps warn — they never block (v0 policy).
";

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
    // Help is answered before anything opens the broker or records a metric:
    // asking what a command does must never do any of it (Phase 4, P4.3).
    if wants_help(args) {
        return match help_text(args) {
            Some(text) => {
                out!("{}", text.trim_end());
                0
            }
            None => {
                eprintln!(
                    "Error: unknown broker subcommand {:?} — see `aethyme broker --help`",
                    args.iter()
                        .find(|arg| !arg.starts_with('-'))
                        .map(String::as_str)
                        .unwrap_or_default()
                );
                crate::exit_status::USAGE
            }
        };
    }
    // Public verbs, `advanced <verb>` and old spellings all dispatch to the
    // internal subcommand that implements them. The router prints the
    // deprecation warning; in-process callers pass internal spellings.
    let resolved = resolve(args);
    if let Some(refusal) = &resolved.refusal {
        eprintln!("Error: {refusal}");
        return crate::exit_status::USAGE;
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
    let args = resolved.args.as_slice();
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
            eprint!("{}", surface::public_help());
            (crate::exit_status::USAGE, false)
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
    /// `submit --verify-only`: run verification and promote nothing.
    verify_only: bool,
    /// `status --summary`: skip the per-session lease refresh (#182).
    summary: bool,
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
    reason: Option<String>,
    agent: Option<String>,
    repo_name: Option<String>,
    tab_name: Option<String>,
    ai_provider: Option<String>,
    pr_number: Option<i64>,
    session: Option<i64>,
    to_session: Option<i64>,
    note_id: Option<i64>,
    message: Option<String>,
    entry: Option<i64>,
    confirm: Option<String>,
    delivery_mode: Option<String>,
    delivery_plan: Option<String>,
    operation: Option<i64>,
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
    all: bool,
    all_cleaned: bool,
    keep_worktree: bool,
    chau7: bool,
    fix_version: bool,
    with_gate: bool,
    apply: bool,
    dry_run: bool,
    include_active_gate_cache: bool,
    destructive: bool,
    no_wait: bool,
    allow_parallel: bool,
    queue_timeout_seconds: Option<u64>,
    break_glass: bool,
    sync_main: bool,
    sync_integration: bool,
    no_cache: bool,
    probe: bool,
    only: Option<String>,
    stdout: bool,
    include_task: bool,
    offline: bool,
    required_mode: Option<String>,
    planned_paths: Vec<String>,
    declared_scopes: Vec<String>,
    exec_command: Vec<String>,
    /// Every flag spelled on the command line, in order, as written (`--` for
    /// the command separator). `validate_flags` checks these against the
    /// subcommand's entry in `FLAG_RULES`.
    given_flags: Vec<String>,
}

fn parse(args: &[String]) -> Result<Parsed, UsageError> {
    let mut parsed = Parsed {
        read_only_snapshot: false,
        verify_only: false,
        summary: false,
        positional: Vec::new(),
        task: None,
        cmd: None,
        target: None,
        repository: None,
        pull_request: None,
        scope: None,
        effect: None,
        outcome: None,
        reason: None,
        agent: None,
        repo_name: None,
        tab_name: None,
        ai_provider: None,
        pr_number: None,
        session: None,
        to_session: None,
        note_id: None,
        message: None,
        entry: None,
        confirm: None,
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
        all: false,
        all_cleaned: false,
        keep_worktree: false,
        chau7: false,
        fix_version: false,
        with_gate: false,
        apply: false,
        dry_run: false,
        include_active_gate_cache: false,
        destructive: false,
        no_wait: false,
        allow_parallel: false,
        queue_timeout_seconds: None,

        break_glass: false,
        sync_main: false,
        sync_integration: false,
        no_cache: false,
        probe: false,
        only: None,
        stdout: false,
        include_task: false,
        offline: false,
        required_mode: None,
        planned_paths: Vec::new(),
        declared_scopes: Vec::new(),
        exec_command: Vec::new(),
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
            "--json" => parsed.json = true,
            "--summary" => parsed.summary = true,
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
            "--keep-worktree" => parsed.keep_worktree = true,
            "--chau7" => parsed.chau7 = true,
            "--fix-version" => parsed.fix_version = true,
            "--with-gate" => parsed.with_gate = true,
            "--apply" => parsed.apply = true,
            "--dry-run" => parsed.dry_run = true,
            "--include-active-gate-cache" => parsed.include_active_gate_cache = true,
            "--from-provider" => parsed.from_provider = true,
            "--no-wait" => parsed.no_wait = true,
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
            "--allow-parallel" => parsed.allow_parallel = true,
            "--break-glass" => parsed.break_glass = true,
            "--sync-main" => parsed.sync_main = true,
            "--sync-integration" => parsed.sync_integration = true,
            "--no-cache" => parsed.no_cache = true,
            "--verify-only" => parsed.verify_only = true,
            "--probe" => parsed.probe = true,
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

fn open_broker(read_only_snapshot: bool) -> Result<Broker, UsageError> {
    let cwd = std::env::current_dir()
        .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
    if read_only_snapshot {
        Ok(Broker::open_snapshot(&cwd)?)
    } else {
        Ok(Broker::open(&cwd)?)
    }
}

/// Starting or adopting a session is the one useful moment to surface a
/// stale installed broker. The check is advisory and writes to stderr so a
/// `--json` session report remains machine-readable.
fn warn_stale_broker_binary(broker: &Broker) {
    if let Some(warning) = crate::version::broker_start_warning(broker.main_root()) {
        eprintln!("warning: {warning}");
    }
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
    // `start` is the one public verb that merges by flag: registering an
    // existing worktree and spawning a process keep their own handlers.
    let subcommand = &match subcommand.as_str() {
        "start" if parsed.adopt || parsed.reuse || parsed.replace_stale => "adopt".to_string(),
        "start" if parsed.cmd.is_some() => "start-agent".to_string(),
        other => other.to_string(),
    };
    // One declarative check replaces the per-flag guards that grew after #285:
    // a flag the subcommand never reads is refused rather than dropped.
    validate_flags(subcommand, &parsed)?;
    surface_command_advisories(subcommand, &parsed);

    match subcommand.as_str() {
        "readiness" => run_readiness(parsed)?,
        "worktree-root" => run_worktree_root(parsed)?,
        "adopt" => run_adopt(parsed)?,
        "start" => run_start(parsed)?,
        "start-agent" => run_start_agent(parsed)?,
        "report" => run_report(parsed)?,
        "quality-report" => run_quality_report(parsed)?,
        "external-events" => run_external_events(parsed)?,
        "reclaim" => run_reclaim(parsed)?,
        "deliveries" => run_deliveries(parsed)?,
        "review" => run_review(parsed)?,
        "prepare" => run_prepare(parsed)?,
        "console" => run_console(parsed)?,
        "resources" => run_resources(parsed)?,
        "agents" => run_agents(parsed)?,
        "leases" => run_leases(parsed)?,
        "exec" => run_exec(parsed)?,
        "git" | "gh" => run_git_gh(parsed, subcommand)?,
        "advisories" => run_advisories(parsed)?,
        "exposures" => run_exposures(parsed)?,
        "note" => run_note(parsed)?,
        "operations" => run_operations(parsed)?,
        "gates" => run_gates(parsed)?,
        "watch" => run_pull_request_watch(parsed)?,
        "pr" => run_pr(parsed)?,
        "hooks" => run_hooks(parsed)?,
        "submit" => run_submit(parsed)?,
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
        "blockers" => run_blockers(parsed)?,
        "unblock" => run_unblock(parsed)?,
        "doctor" => run_doctor(parsed)?,
        "quick-test" => run_quick_test(parsed)?,
        "trust" => run_trust_command(&parsed)?,
        "verify-loop" | "e2e" => run_verify_loop(parsed)?,
        "init" => run_init(parsed)?,
        "certify" | "scaffold" => run_certify_scaffold(parsed, subcommand)?,
        "handoff" => run_handoff(parsed)?,
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
