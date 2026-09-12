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

const RESOURCES_RECONCILE_USAGE: &str =
    "usage: aethyme broker resources reconcile <lease-id> --confirm <generation> [--json]";
const OPERATIONS_RECONCILE_USAGE: &str = "usage: aethyme broker operations reconcile \
     --operation <id> --outcome <succeeded|failed> --reason <text> [--json]";
const OPERATIONS_SHOW_USAGE: &str = "usage: aethyme broker operations show <id> [--json]";
const ADVISORIES_SHOW_USAGE: &str = "usage: aethyme broker advisories show <id> [--json]";
const ADVISORIES_ACK_USAGE: &str = "usage: aethyme broker advisories ack <id> [--json]";
const ADVISORIES_SUPPRESS_USAGE: &str = "usage: aethyme broker advisories suppress <id> [--json]";
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
  aethyme broker adopt [<path>] [--task <text>] [--path <repo-path>]... [--agent <name-and-email>] [--reuse [--sync-integration]|--replace-stale] [--json]
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
  aethyme broker start --task <text> [--path <repo-path>]... [--agent <name-and-email>] [--json]
      Create a broker-managed worktree + branch and register a session,
      atomically claiming every reviewed --path, but do not spawn a process.
      Prefer this over adopting the main
      checkout for agent work; it isolates the git index and worktree.
      --agent as in adopt (see above).
  aethyme broker start-agent --task <text> --cmd <command>
                             [--agent <identity>] [--json]
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
  aethyme broker console [status] [--json]
      Show which consoles are serving this repository right now, this
      repository's console mode, and whether this checkout is the canonical
      one. Read-only; reserves nothing.
  aethyme broker console plan [--json]
      Show exactly what a console launch would reserve under the current mode
      without reserving it.
  aethyme broker console run [--wait <duration>] [--json] -- <command> ...
      Run a dev server under the repository's console mode. `singular` takes
      one exclusive key and one pinned port, so a second launch anywhere on
      this host is refused with the console that already holds it.
      `per_worktree` takes a distinct port, namespace, and one slot from a
      bounded pool, so worktrees run side by side without exhausting the host.
      `unmanaged` reserves nothing and executes directly. Allocations reach
      the command as AETHYME_RESOURCE_PORT, AETHYME_RESOURCE_NAMESPACE, and
      AETHYME_RESOURCE_SLOT. Configure with [console] in .aethyme/config.toml:
      mode, port, port_end, pool_limit, ttl_seconds.
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
      (hooks pre-commit / hooks post-commit / hooks pre-push are internal entry
      points the installed shims call — not for direct use.)
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
      Print the review ledger: every review the router has requested, for
      which head, through which backend, and how it ended. This is the answer
      to \"why was there no review\" -- read-only, needs no session.
  aethyme broker review state --repo <owner/name> --pr <number> --type <review-type> --state <state> [--head <sha>] [--note <text>] [--json]
      Report what became of one requested review: running, satisfied, failed,
      recorded, or abandoned. The broker decides and records; whoever performs
      the review closes the row here, which is what drains the router's
      concurrency slots. --head targets a superseded commit; the default is
      the most recent request for that review type.
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
      queue, integration head. Session records are reported under the
      `agents` key; the id every `--session <id>` flag expects is
      `agents[].id`. There is no `sessions` key.
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
/// `--agent` when given, else `AETHYME_AGENT`. Resolved in the agent's own
/// process, because promotion can run from a different one (issue rescue: a
/// later `submit` or queue drain would otherwise credit whoever triggered it).
fn session_agent_identity(explicit: Option<&str>) -> Option<String> {
    explicit
        .map(str::to_string)
        .or_else(|| crate::attribution::agent_from_env().map(|identity| identity.render()))
}

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
    "console",
    "prepare",
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
    "promotion-record",
    "representation",
    "scan",
    "record",
    "main",
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
    "readiness",
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

/// Record one command's cost. `output_bytes` counts stdout emitted through
/// `out!`; stderr is excluded because it carries diagnostics rather than the
/// payload an agent pays to read.
fn record_command_metric(args: &[String], exit: u8, duration_ms: i64) {
    let output_bytes = crate::cli_output::emitted();
    // Inspection commands are contractually side-effect free: the CLI documents
    // "never writes broker state or command telemetry" and `external_events_cli`
    // asserts the metrics file is byte-identical across them. That invariant
    // also hides the commands that dominate agent token cost, because reads are
    // the frequent, expensive ones. Measuring them is therefore opt-in: unset,
    // nothing changes; set, the operator has accepted that inspection now
    // writes one telemetry line.
    if !command_records_metric(args) && !output_measurement_opted_in() {
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
        "{{\"ts\":{ts},\"command\":\"{}\",\"duration_ms\":{duration_ms},\"exit\":{exit},\"output_bytes\":{output_bytes}}}\n",
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
/// Whether the operator opted into measuring read-only command output.
///
/// Off by default so inspection stays side-effect free. `AETHYME_MEASURE_OUTPUT`
/// is read per invocation rather than cached, so enabling it needs no restart of
/// anything and a wrapper can scope it to a single command.
fn output_measurement_opted_in() -> bool {
    std::env::var_os("AETHYME_MEASURE_OUTPUT")
        .map(|value| {
            let value = value.to_string_lossy().to_ascii_lowercase();
            !matches!(value.as_str(), "" | "0" | "false" | "no" | "off")
        })
        .unwrap_or(false)
}

fn command_records_metric(args: &[String]) -> bool {
    match args.first().map(String::as_str) {
        Some("certify" | "readiness" | "queue" | "metrics" | "handoff" | "worktree-root") => false,
        Some("advisories") => matches!(args.get(1).map(String::as_str), Some("ack" | "suppress")),
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
        Some("representation") => args.get(1).map(String::as_str) == Some("record"),
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
        Some("console") => args.get(1).map(String::as_str) == Some("run"),
        Some("resources") => !matches!(args.get(1).map(String::as_str), Some("plan" | "list")),
        Some("events") => args.get(1).map(String::as_str) == Some("prune"),
        Some("gates") => match args.get(1).map(String::as_str) {
            Some("validate" | "manifest" | "scope" | "affected" | "semantic") => false,
            Some("doctor") => args.iter().any(|arg| arg == "--probe"),
            _ => true,
        },
        Some("doctor") => args.iter().any(|arg| arg == "--fix-version"),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::repository_wide_publication_lines;

    fn assessment(fast_forward: bool, dirty: &[&str]) -> crate::ship::ShipLocalMainSyncAssessment {
        crate::ship::ShipLocalMainSyncAssessment {
            safe: fast_forward && dirty.is_empty(),
            current_branch_matches: true,
            local_head_unchanged: true,
            fast_forward,
            tracked_dirty_paths: dirty.iter().map(|p| (*p).into()).collect(),
            untracked_paths: Vec::new(),
            conflicting_untracked_paths: Vec::new(),
        }
    }

    /// Issue #141: "Freshness: Ready" describes the prefix, not the repository.
    /// An operator asking to publish everything must be told what is omitted.
    #[test]
    fn ship_plan_states_whether_the_prefix_represents_all_local_work() {
        let complete = repository_wide_publication_lines(
            &assessment(true, &[]),
            "refs/heads/main",
            "aaaa",
            "aaaa",
        );
        assert_eq!(complete.len(), 1);
        assert!(complete[0].contains("complete"), "{complete:?}");

        let diverged = repository_wide_publication_lines(
            &assessment(false, &[]),
            "refs/heads/main",
            "aaaa",
            "bbbb",
        )
        .join("\n");
        assert!(diverged.contains("INCOMPLETE"), "{diverged}");
        assert!(
            diverged.contains("git log --oneline aaaa..bbbb"),
            "the operator must be able to list what is excluded: {diverged}"
        );

        let dirty = repository_wide_publication_lines(
            &assessment(true, &["src/a.rs", "src/b.rs"]),
            "refs/heads/main",
            "aaaa",
            "aaaa",
        )
        .join("\n");
        assert!(dirty.contains("INCOMPLETE"), "{dirty}");
        assert!(
            dirty.contains("2 uncommitted tracked path(s): src/a.rs, src/b.rs"),
            "uncommitted tracked work must never be silently omitted: {dirty}"
        );
    }

    /// On another branch, local main says nothing about completeness.
    #[test]
    fn a_different_checked_out_branch_makes_no_completeness_claim() {
        let mut other = assessment(true, &[]);
        other.current_branch_matches = false;
        assert!(
            repository_wide_publication_lines(&other, "refs/heads/main", "aaaa", "bbbb").is_empty()
        );
    }

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
            args(&["readiness", "--require", "agent-ready"]),
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
            args(&["gates", "doctor"]),
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
            args(&["gates", "doctor", "--probe"]),
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
            args(&["advisories", "suppress", "1"]),
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
    fn parse_accepts_readiness_requirement() {
        let args = ["--require".into(), "parallel-ready".into(), "--json".into()];
        let parsed = match super::parse(&args) {
            Ok(parsed) => parsed,
            Err(_) => panic!("readiness flags should parse"),
        };
        assert_eq!(parsed.required_mode.as_deref(), Some("parallel-ready"));
        assert!(parsed.json);
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
    fn parse_accepts_explicit_gate_doctor_probe() {
        let parsed = match super::parse(&args(&[
            "doctor",
            "--probe",
            "--only",
            "integration",
            "--json",
        ])) {
            Ok(parsed) => parsed,
            Err(_) => panic!("gate doctor probe flags should parse"),
        };
        assert_eq!(parsed.positional, vec!["doctor"]);
        assert!(parsed.probe);
        assert_eq!(parsed.only.as_deref(), Some("integration"));
        assert!(parsed.json);
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

    /// `review state` is the reviewer's half of the handoff, and the reviewer
    /// is a script. `--detail` was already a boolean elsewhere, so the free
    /// text is `--note`; this pins that choice against a future rename that
    /// would silently drop the text.
    #[test]
    fn parse_accepts_the_review_ledger_report_flags() {
        let args = [
            "review",
            "state",
            "--repo",
            "Owner/Repo",
            "--pr",
            "42",
            "--type",
            "security",
            "--state",
            "satisfied",
            "--head",
            "abc123",
            "--note",
            "no findings",
        ]
        .map(String::from)
        .to_vec();
        let parsed = match super::parse(&args) {
            Ok(parsed) => parsed,
            Err(_) => panic!("review state flags should parse"),
        };
        assert_eq!(parsed.positional, vec!["review", "state"]);
        assert_eq!(parsed.repository.as_deref(), Some("Owner/Repo"));
        assert_eq!(parsed.pr_number, Some(42));
        assert_eq!(parsed.review_type.as_deref(), Some("security"));
        assert_eq!(parsed.review_state.as_deref(), Some("satisfied"));
        assert_eq!(parsed.head.as_deref(), Some("abc123"));
        assert_eq!(parsed.note.as_deref(), Some("no findings"));
        assert!(!parsed.detail, "--detail stays the boolean it already was");
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
    fn parse_accepts_foreground_pr_scheduler_limit() {
        let args = vec![
            "pr".to_string(),
            "tick".to_string(),
            "--limit".to_string(),
            "17".to_string(),
            "--json".to_string(),
        ];
        let parsed = match super::parse(&args) {
            Ok(parsed) => parsed,
            Err(_) => panic!("scheduler tick flags should parse"),
        };
        assert_eq!(parsed.positional, vec!["pr", "tick"]);
        assert_eq!(parsed.limit, Some(17));
        assert!(parsed.json);
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

/// Cloned per pull request by `review tick`, which routes many of them from one
/// parse of one command line.
#[derive(Clone)]
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
    no_wait: bool,
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
        no_wait: false,
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
            "--detail" => parsed.detail = true,
            "--active" => parsed.active = true,
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
            "--break-glass" => parsed.break_glass = true,
            "--sync-main" => parsed.sync_main = true,
            "--sync-integration" => parsed.sync_integration = true,
            "--no-cache" => parsed.no_cache = true,
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

fn render_readiness_report(report: &crate::ReadinessReport, json: bool) -> Result<(), UsageError> {
    if json {
        out!("{}", crate::render_readiness_json(report)?);
    } else {
        out!("{}", crate::render_readiness_text(report).trim_end());
    }
    Ok(())
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
        out!("{tag:<8} {:<28} {}", check.id, check.detail);
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
        out!("{}", serde_json::to_string_pretty(reports)?);
    } else {
        for report in reports {
            out!(
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
        out!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }

    for path in &report.paths {
        out!(
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
                out!(
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
                        out!("    next: {action}");
                    }
                }
            }
        }
        if path.owned.is_empty() && path.conflicts.is_empty() {
            out!("  no active overlaps");
        }
    }
    Ok(())
}

fn render_planned_explicit_leases(leases: &[crate::Lease]) {
    if leases.is_empty() {
        return;
    }
    out!("Planned explicit leases:");
    for lease in leases {
        out!("  {}", lease.path);
    }
}

fn render_worktree_placement(placement: &crate::WorktreePlacement) {
    let boundary = if placement.outside_repository {
        "outside the repository"
    } else {
        "inside the repository fallback"
    };
    out!(
        "Worktree root: {} ({}, {boundary})",
        placement.root.display(),
        placement.source.as_str()
    );
    if let Some(reason) = &placement.fallback_reason {
        out!("Warning: external worktree placement was unavailable: {reason}");
    }
}

fn render_preparation_status(
    status: &crate::PreparationStatus,
    json: bool,
) -> Result<(), UsageError> {
    if json {
        out!("{}", serde_json::to_string_pretty(status)?);
        return Ok(());
    }
    out!(
        "Preparation {:?} for session {}: {}",
        status.state,
        status.session_id,
        status.reason
    );
    if let Some(digest) = &status.expected_digest {
        out!("Expected digest: {}", short_sha(digest));
    }
    if !status.missing_outputs.is_empty() {
        out!("Missing outputs:");
        for path in &status.missing_outputs {
            out!("  {path}");
        }
    }
    if let Some(next_action) = &status.next_action {
        out!("Next: {next_action}");
    }
    Ok(())
}

fn render_pr_check_report(report: &crate::PrCheckReport, json: bool) -> Result<(), UsageError> {
    if json {
        out!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }

    match &report.pr {
        Some(pr) => {
            out!(
                "PR #{} -> {}: {}",
                pr.number,
                report.target_branch,
                pr.title
            );
            if let Some(url) = &pr.url {
                out!("URL: {url}");
            }
        }
        None => {
            out!("{}", report.decision.summary);
        }
    }
    out!("Marker: {}", report.marker.as_str());
    out!(
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
    out!("Decision: {}", report.decision.summary);
    out!(
        "Dispatch: {}{}",
        report.dispatch.status.as_str(),
        report
            .dispatch
            .session_id
            .map(|id| format!(" (session {id})"))
            .unwrap_or_default()
    );
    if let Some(path) = &report.prompt_path {
        out!("Prompt: {path}");
    }
    for command in &report.next_commands {
        out!("run: {command}");
    }
    Ok(())
}

fn render_quick_test_report(report: &crate::QuickTestReport, json: bool) -> Result<(), UsageError> {
    if json {
        out!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    if report.skipped {
        out!("{}", report.message);
        return Ok(());
    }
    out!("{}", report.message);
    if report.chau7.detected {
        out!(
            "Chau7 runtime markers detected: {}",
            report.chau7.markers.join(", ")
        );
    }
    for step in &report.steps {
        out!("{:<8} {:<20} {}", step.status, step.name, step.detail);
    }
    if let Some(gate) = &report.gate_fixture {
        out!("gate fixture: {}", gate.gate_name);
        out!("  passing entry: q{}", gate.passing_entry_id);
        for outcome in &gate.passing_outcomes {
            out!(
                "    {} {}{} (tree {})",
                outcome.gate,
                outcome.status.as_str(),
                if outcome.cached { " (cached)" } else { "" },
                short_commit(&outcome.tree_hash),
            );
        }
        out!(
            "  failing entry: q{} ({})",
            gate.failing_entry_id,
            gate.failing_entry_status.as_str()
        );
        for outcome in &gate.failing_outcomes {
            out!(
                "    {} {}{} (tree {})",
                outcome.gate,
                outcome.status.as_str(),
                if outcome.cached { " (cached)" } else { "" },
                short_commit(&outcome.tree_hash),
            );
        }
    }
    out!(
        "temporary repo removed: {}",
        if report.temp_repo_removed {
            "yes"
        } else {
            "no"
        }
    );
    if let Some(head) = &report.integration_head {
        out!("integration head: {}", &head[..12.min(head.len())]);
    }
    Ok(())
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
    out!("Next actions:");
    if advice.is_empty() {
        out!("  none");
        return;
    }
    for (index, item) in advice.iter().enumerate() {
        out!(
            "  {}. {:<7} {}",
            index + 1,
            item.severity.as_str().to_uppercase(),
            item.summary
        );
        if !item.evidence.is_empty() {
            out!("     evidence: {}", item.evidence.join("; "));
        }
        for command in &item.commands {
            out!("     run: {command}");
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
        out!("No terminal merge-queue entries in this page.");
    } else {
        out!("{:<4} {:<4} {:<17} HEAD", "ID", "SID", "STATUS");
        for entry in &page.entries {
            out!(
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
    out!(
        "Terminal totals: {}",
        if summary.is_empty() { "none" } else { &summary }
    );
    if let Some(before) = page.next_before_id {
        out!("Next: aethyme broker queue history --before {before}");
    }
}

fn render_repair_report(report: &crate::RepairReport) {
    out!(
        "Repair session {}: {}",
        report.session_id,
        report.action.as_str()
    );
    out!("  source: {}", report.source.as_str());
    if let Some(base) = &report.base {
        out!("  base: {}", &base[..12.min(base.len())]);
    }
    if report.pending_commits.is_empty() {
        out!("  pending commits: none");
    } else {
        out!("  pending commits:");
        for commit in &report.pending_commits {
            out!("    - {commit}");
        }
    }
    out!(
        "  leases refreshed: {}",
        if report.leases_refreshed { "yes" } else { "no" }
    );
    if report.affected_gates.is_empty() {
        out!("  affected gates: none");
    } else {
        out!("  affected gates:");
        for gate in &report.affected_gates {
            match &gate.triggered_by {
                Some(path) => out!("    - {} (triggered by {})", gate.gate, path),
                None => out!("    - {} (always runs)", gate.gate),
            }
        }
    }
    out!("  next: {}", report.next_command);
}

fn render_finish_report(report: &crate::FinishReport) {
    out!(
        "Finish session {}: {}",
        report.session_id,
        report.status.as_str()
    );
    out!("  {}", report.summary);
    out!("  worktree: {}", report.worktree_path);
    if let Some(entry_id) = report.latest_queue_entry_id {
        let status = report
            .latest_queue_status
            .map(|status| status.as_str())
            .unwrap_or("unknown");
        out!("  latest queue: qid {entry_id} ({status})");
    }
    out!(
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
        out!("  dirty paths: {}", capped_join(&report.dirty_paths, 8));
    }
    if report.unsubmitted_commits > 0 {
        out!("  unsubmitted commits: {}", report.unsubmitted_commits);
    }
    out!(
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
        out!("  leases held: none recorded");
    } else {
        // Past tense once closed: closing released these, and the list is
        // handoff history rather than a claim of current ownership (#141).
        if report.cleanup.completed {
            out!("  leases at close:");
        } else {
            out!("  leases held:");
        }
        for lease in &report.leases_held {
            out!(
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
        Some(gate) => out!(
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
        None => out!("  last gate: none recorded"),
    }
    match &report.last_graph_integrity {
        Some(graph) => out!(
            "  last graph integrity: {} on tree {} under policy {} at {}",
            graph.status.as_str(),
            short_commit(&graph.tree_hash),
            short_commit(&graph.policy_digest),
            graph.recorded_at,
        ),
        None => out!("  last graph integrity: none recorded"),
    }
    out!(
        "  cleanup safe: {}",
        if report.cleanup_safe { "yes" } else { "no" }
    );
    out!(
        "  physical cleanup: requested={}, kept={}, attempted={}, completed={}, reclaimed={} bytes",
        report.cleanup.requested,
        report.cleanup.kept,
        report.cleanup.attempted,
        report.cleanup.completed,
        report.cleanup.reclaimed_bytes,
    );
    out!(
        "    worktree: {} ({})",
        report.worktree_path,
        if report.cleanup.worktree_removed {
            "removed"
        } else {
            "retained"
        }
    );
    if let Some(branch) = &report.cleanup.branch_ref {
        out!(
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
        out!("    recovery: {action}");
    }
    for warning in &report.warnings {
        out!("  warning: {warning}");
    }
    if report.next_commands.is_empty() {
        out!("  next: none");
    } else {
        out!("  next:");
        for command in &report.next_commands {
            out!("    run: {command}");
        }
    }
    out!(
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

fn render_cleanup_sweep_report(report: &crate::CleanupSweepReport, detail: bool) {
    out!(
        "Cleanup {}: {} retained broker-owned worktrees, {} eligible",
        if report.applied { "apply" } else { "plan" },
        report.plan.retained_worktree_count,
        report.plan.eligible_worktree_count,
    );
    out!(
        "  retained: {}; reclaimable now: {}; branches: {} retained, {} eligible",
        human_bytes(report.plan.estimated_retained_bytes),
        human_bytes(report.plan.estimated_reclaimable_bytes),
        report.plan.retained_branch_count,
        report.plan.eligible_branch_count,
    );
    out!("  reviewed plan digest: {}", report.plan.digest);
    render_capped(&report.plan.worktrees, GC_LIST_CAP, detail, |item| {
        out!(
            "  session {}: {} ({}) — {}",
            item.session_id,
            item.disposition.as_str(),
            item.estimated_bytes
                .map(human_bytes)
                .unwrap_or_else(|| "size unavailable".into()),
            item.reason,
        );
        out!("    {}", item.worktree_path);
        if let Some(branch_tip) = &item.branch_tip {
            out!("    {} at {}", item.branch_ref, branch_tip);
        }
        for command in &item.inspection_commands {
            out!("    inspect: {command}");
        }
        if !item.eligible() {
            out!("    explicit discard: {}", item.force_cleanup_command);
        }
    });
    if report.applied {
        out!(
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
            out!(
                "  retained session {} after revalidation: {}",
                failure.session_id,
                failure.reason
            );
        }
    } else if report.plan.eligible_worktree_count > 0 || report.plan.eligible_branch_count > 0 {
        out!(
            "  apply: aethyme broker cleanup --all-cleaned --apply --confirm {}",
            report.plan.digest
        );
    }
}

fn render_promotion_record_plan(plan: &crate::PromotionRecordPlan) {
    let recoverable = plan.recoverable().count();
    out!(
        "Promotion record plan {}: {} unrecorded commit(s), {} recoverable",
        plan.digest,
        plan.candidates.len(),
        recoverable
    );
    out!(
        "  integration: {} @ {}",
        plan.integration_ref,
        plan.integration_tip
    );
    for candidate in &plan.candidates {
        match (&candidate.entry_id, &candidate.blocker) {
            (Some(entry), None) => {
                out!(
                    "  {} -> entry {} (session {}), currently {}",
                    candidate.commit,
                    entry,
                    candidate.session_id.unwrap_or_default(),
                    candidate.current_status.as_deref().unwrap_or("unknown")
                );
                for line in &candidate.evidence {
                    out!("      evidence: {line}");
                }
            }
            _ => {
                out!("  {} -> not recoverable", candidate.commit);
                if let Some(blocker) = &candidate.blocker {
                    out!("      blocked: {blocker}");
                }
            }
        }
    }
    if recoverable == 0 {
        out!("  apply: nothing recoverable");
    } else {
        out!(
            "  apply: aethyme broker promotion-record apply --confirm {}",
            plan.digest
        );
    }
}

/// Default number of items any one plan list prints before summarising.
const GC_LIST_CAP: usize = 5;

/// Ship plans list every promoted entry in the prefix; show the boundary only.
const SHIP_ENTRY_CAP: usize = 6;

/// Print at most `cap` items, then say how many were withheld.
///
/// Agent-facing output is charged per token. A plan that enumerates every
/// finding costs the reader far more than the decision it supports: this
/// repository's `gc plan` reached ~319 KB, of which 93% was one list. The
/// counts and the digest are what a reader acts on; the enumeration is what
/// they page past. `--detail` restores it when someone genuinely wants to audit.
/// "Ready" means this exact prefix is safe to push. It does not mean the prefix
/// represents every piece of work in the repository, and an operator asking to
/// "publish everything" reasonably reads it that way (issue #141).
fn repository_wide_publication_lines(
    assessment: &crate::ship::ShipLocalMainSyncAssessment,
    local_default_branch_ref: &str,
    publication_sha: &str,
    local_default_branch_sha: &str,
) -> Vec<String> {
    if !assessment.current_branch_matches {
        // The primary checkout is on another branch, so local main says nothing
        // about completeness here.
        return Vec::new();
    }
    let excluded_commits = !assessment.fast_forward;
    let dirty = assessment.tracked_dirty_paths.len();
    if !excluded_commits && dirty == 0 {
        return vec![
            "Repository-wide publication: complete (this prefix represents local main)".into(),
        ];
    }
    let mut lines = vec!["Repository-wide publication: INCOMPLETE".into()];
    if excluded_commits {
        lines.push(format!(
            "  excluded: local {} carries commits this prefix does not contain; list them with `git log --oneline {}..{}`",
            local_default_branch_ref, publication_sha, local_default_branch_sha,
        ));
    }
    if dirty > 0 {
        lines.push(format!(
            "  excluded: {dirty} uncommitted tracked path(s): {}",
            assessment.tracked_dirty_paths.join(", ")
        ));
    }
    lines.push("  publishing now is safe, but omits the work listed above".into());
    lines
}

fn load_main_reconcile_resolutions(
    parsed: &Parsed,
) -> Result<Option<crate::MainReconcileResolutionDocument>, UsageError> {
    let Some(path) = parsed.resolution_file.as_deref() else {
        return Ok(None);
    };
    let text = std::fs::read_to_string(path).map_err(|source| {
        UsageError::Message(format!("cannot read {}: {source}", path.display()))
    })?;
    let document: crate::MainReconcileResolutionDocument = serde_json::from_str(&text)
        .map_err(|source| UsageError::Message(format!("invalid {}: {source}", path.display())))?;
    Ok(Some(document))
}

fn render_main_reconcile_plan(plan: &crate::MainReconcilePlan, detail: bool) {
    out!(
        "Main reconcile plan {}: {} local-only commit(s) on {}",
        plan.digest,
        plan.commits.len(),
        plan.default_branch
    );
    out!("  local:       {} @ {}", plan.local_ref, plan.local_sha);
    out!(
        "  integration: {} @ {}",
        plan.integration_ref,
        plan.integration_sha
    );
    let unrepresented = plan.unrepresented().count();
    out!(
        "  {} already represented, {} unrepresented",
        plan.commits.len() - unrepresented,
        unrepresented
    );
    if !plan.dirty_tracked_paths.is_empty() {
        out!(
            "  uncommitted tracked path(s): {}",
            plan.dirty_tracked_paths.join(", ")
        );
    }
    // Unrepresented commits are the decision; represented ones are the evidence
    // that moving the branch is safe, and are summarised unless asked for.
    for commit in plan.unrepresented() {
        out!(
            "  unrepresented {} {} — {}{}",
            &commit.commit[..12.min(commit.commit.len())],
            commit.subject,
            commit.evidence,
            match commit.resolution {
                Some(resolution) => format!(" [{}]", resolution.as_str()),
                None => " [no decision recorded]".into(),
            }
        );
    }
    if detail {
        for commit in plan
            .commits
            .iter()
            .filter(|item| item.disposition == crate::MainReconcileDisposition::AlreadyRepresented)
        {
            out!(
                "  represented   {} {} — {}",
                &commit.commit[..12.min(commit.commit.len())],
                commit.subject,
                commit.evidence
            );
        }
    }
    match &plan.refusal {
        Some(refusal) => out!("  refusal: {refusal}"),
        None => {
            out!("  preservation ref: {}", plan.preservation_ref);
            out!(
                "  apply: aethyme broker main reconcile apply --session <id> --confirm {}",
                plan.digest
            );
        }
    }
}

fn render_capped<T>(items: &[T], cap: usize, detail: bool, mut render: impl FnMut(&T)) {
    let shown = if detail {
        items.len()
    } else {
        cap.min(items.len())
    };
    for item in &items[..shown] {
        render(item);
    }
    if shown < items.len() {
        out!(
            "    ... and {} more; rerun with --detail to list them",
            items.len() - shown
        );
    }
}

fn render_gc_plan(plan: &crate::GcPlan, detail: bool) {
    out!(
        "GC plan {}: {} rows, {} files, {} represented worktrees, {} build caches, {} orphaned roots, {} reclaimable",
        plan.digest,
        plan.rows.len(),
        plan.files.len(),
        plan.worktrees.len(),
        plan.artifacts.len(),
        plan.orphans.len(),
        human_bytes(plan.estimated_reclaimable_bytes),
    );
    out!(
        "  retained: {}; blocked by policy or provenance: {}",
        human_bytes(plan.estimated_retained_bytes),
        human_bytes(plan.estimated_blocked_bytes),
    );
    render_capped(&plan.rows, GC_LIST_CAP, detail, |row| {
        out!(
            "  row: {:?} {} at {} ({} bytes)",
            row.kind,
            row.id,
            row.recorded_at,
            row.estimated_bytes
        );
    });
    render_capped(&plan.files, GC_LIST_CAP, detail, |file| {
        out!(
            "  file: {:?} {} ({} -> {} bytes; before {})",
            file.action,
            file.path,
            file.bytes_before,
            file.bytes_after,
            file.before_sha256
        );
    });
    render_capped(&plan.worktrees, GC_LIST_CAP, detail, |worktree| {
        out!(
            "  worktree: session {} {} ({} bytes)",
            worktree.session_id,
            worktree.worktree_path,
            worktree.estimated_bytes
        );
        out!(
            "    ref: {} at {}",
            worktree.branch_ref,
            worktree.branch_tip.as_deref().unwrap_or("missing")
        );
    });
    render_capped(&plan.artifacts, GC_LIST_CAP, detail, |artifact| {
        out!(
            "  build cache: session {} {}/{} ({}, idle {} days)",
            artifact.session_id,
            artifact.worktree_path,
            artifact.relative_dir,
            human_bytes(artifact.estimated_bytes),
            artifact.idle_days,
        );
    });
    render_capped(&plan.orphans, GC_LIST_CAP, detail, |orphan| {
        out!(
            "  orphaned root: {} ({}) — {}",
            orphan.worktree_root,
            human_bytes(orphan.estimated_bytes),
            orphan.reason,
        );
        out!(
            "    owning repository: {} (missing)",
            orphan.repository_root
        );
    });
    render_capped(&plan.blockers, GC_LIST_CAP, detail, |blocker| {
        out!(
            "  protected: {}{} — {}",
            blocker.kind,
            blocker.id.map(|id| format!(" {id}")).unwrap_or_default(),
            blocker.reason
        );
    });
    if plan.rows.is_empty()
        && plan.files.is_empty()
        && plan.worktrees.is_empty()
        && plan.artifacts.is_empty()
        && plan.orphans.is_empty()
    {
        out!("  apply: nothing eligible");
    } else {
        out!("  apply: aethyme broker gc apply --confirm {}", plan.digest);
    }
}

fn render_gc_apply(report: &crate::GcApplyReport) {
    out!(
        "GC apply {}: {} rows, {} files, {} worktrees, {} build caches, {} orphaned roots, {} reclaimed",
        if report.complete {
            "complete"
        } else {
            "paused"
        },
        report.rows_removed,
        report.files_completed.len(),
        report.sessions_cleaned.len(),
        report.artifacts_reclaimed.len(),
        report.orphans_removed.len(),
        human_bytes(report.reclaimed_bytes),
    );
    for failure in &report.failures {
        out!("  retained: {failure}");
    }
    if let Some(action) = &report.recovery_action {
        out!("  recovery: {action}");
    }
}

fn render_handoff_report(report: &crate::SessionHandoffReport) {
    let handoff = &report.handoff;
    out!(
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
        out!("  latest queue: qid {entry_id} ({status})");
    }
    out!(
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
    out!(
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
    out!(
        "  leases: {} recorded ({} active, {} released, {} expired)",
        handoff.leases_held.len(),
        active,
        released,
        expired
    );
    match &handoff.last_gate {
        Some(gate) => out!(
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
        None => out!("  last gate: none recorded"),
    }
    match &handoff.last_graph_integrity {
        Some(graph) => out!(
            "  last graph integrity: {} on tree {} under policy {} at {}",
            graph.status.as_str(),
            short_commit(&graph.tree_hash),
            short_commit(&graph.policy_digest),
            graph.recorded_at,
        ),
        None => out!("  last graph integrity: none recorded"),
    }
    out!(
        "  cleanup safe: {}",
        if handoff.cleanup_safe { "yes" } else { "no" }
    );
    out!(
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
    out!(
        "Broker verify-loop: {}",
        if report.ok { "passed" } else { "failed" }
    );
    out!(
        "  integration tested: {} @ {}",
        report.integration_branch,
        short_commit(&report.tested_integration_head)
    );
    out!(
        "  integration current: {} @ {}",
        report.integration_branch,
        short_commit(&report.current_integration_head)
    );
    if report.integration_moved {
        out!(
            "  warning: integration moved during verification; tested old tip {}, current tip {}; rerun needed",
            short_commit(&report.tested_integration_head),
            short_commit(&report.current_integration_head)
        );
    }
    out!("Steps:");
    for step in &report.steps {
        out!(
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
        out!("  quick-test temp integration: {}", short_commit(head));
    }
    if let Some(doctor) = &report.doctor {
        out!(
            "  doctor version: {} — {}",
            doctor.version.status.as_str(),
            doctor.version.message
        );
    }
    if report.source_tests.attempted {
        out!(
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
                out!("    {line}");
            }
        }
    }
    if report.ok {
        out!("Next: none");
    } else if report.integration_moved {
        out!("Next: rerun `aethyme broker verify-loop` on the current integration tip.");
    } else {
        out!("Next: fix the failed step above, then rerun `aethyme broker verify-loop`.");
    }
}

struct CliGateDoctorProgress;

impl crate::GateProgressSink for CliGateDoctorProgress {
    fn report(&self, line: &str) {
        eprintln!("{line}");
    }
}

fn render_gate_doctor(report: &crate::GateDoctorReport) {
    out!(
        "Gate doctor: advisory only at {}",
        short_commit(&report.source_head)
    );
    out!(
        "  repository: {} tracked file(s), {} source file(s)",
        report.tracked_file_count,
        report.source_file_count
    );
    out!("  gates:");
    for gate in &report.gates {
        out!(
            "    [{}] {} — timeout {}; coverage {}% ({}/{} source paths)",
            gate.cost,
            gate.name,
            gate.timeout_seconds
                .map(|seconds| format!("{seconds}s"))
                .unwrap_or_else(|| "unbounded".into()),
            gate.repository_coverage_percent,
            gate.matched_source_paths,
            report.source_file_count,
        );
    }
    if report.findings.is_empty() {
        out!("  findings: none");
    } else {
        out!("  findings:");
        for finding in &report.findings {
            out!(
                "    {:?}/{:?} {:?}{} — {}",
                finding.severity,
                finding.confidence,
                finding.id,
                finding
                    .gate
                    .as_ref()
                    .map(|gate| format!(" [{gate}]"))
                    .unwrap_or_default(),
                finding.summary,
            );
            for evidence in &finding.evidence {
                out!("      evidence: {evidence}");
            }
            out!("      next: {}", finding.remediation);
        }
    }
    if let Some(probe) = &report.probe {
        out!(
            "  probe: {}",
            if probe.passed {
                "passed"
            } else {
                "did not pass"
            }
        );
        out!(
            "    exact HEAD: {}",
            short_commit(&probe.worktree.exact_head)
        );
        out!("    selected: {}", probe.selected_gates.join(", "));
        out!("    normal result cache: untouched");
        for outcome in &probe.outcomes {
            out!(
                "    {}: {}{}",
                outcome.gate,
                gate_status_label(outcome.status, outcome.failure_class),
                outcome
                    .duration_ms
                    .map(|duration| format!(" in {duration}ms"))
                    .unwrap_or_default(),
            );
        }
        for (kind, paths) in [
            ("tracked", &probe.mutations.tracked),
            ("untracked", &probe.mutations.untracked),
            ("ignored", &probe.mutations.ignored),
        ] {
            if !paths.is_empty() {
                out!("    {kind} mutations: {}", capped_join(paths, 8));
            }
        }
    } else {
        out!("  probe: not run (use --probe explicitly)");
    }
}

fn render_semantic_gate_advice(report: &crate::SemanticGateAdvice) {
    out!("Semantic gate selection: advisory only");
    out!("  session: {}", report.session_id);
    out!("  enforced by this command: no");
    if report.changed_files.is_empty() {
        out!("  changed files: none");
    } else {
        out!("  changed files: {}", capped_join(&report.changed_files, 8));
    }
    out!(
        "  semantic source: {} ({})",
        report.semantic.provider,
        report.semantic.status.as_str()
    );
    out!("    {}", report.semantic.reason);
    if !report.semantic.impacted_paths.is_empty() {
        out!(
            "  semantic impact paths: {}",
            capped_join(&report.semantic.impacted_paths, 8)
        );
    }
    if report.semantic.truncated {
        out!(
            "  semantic impact result: truncated at {} paths",
            report.semantic.result_limit
        );
    }

    if report.path_selected_gates.is_empty() {
        out!("  path-selected gates: none");
    } else {
        out!("  path-selected gates:");
        for gate in &report.path_selected_gates {
            match &gate.triggered_by {
                Some(path) => out!("    - {} (triggered by {})", gate.gate, path),
                None => out!("    - {} (always runs)", gate.gate),
            }
        }
    }

    if report.semantic_suggested_gates.is_empty() {
        out!("  semantic suggestions: none");
    } else {
        out!("  semantic suggestions:");
        for gate in &report.semantic_suggested_gates {
            match &gate.chain {
                Some(chain) => out!(
                    "    - {} ({} -> {} -> {})",
                    gate.gate,
                    chain.changed_file,
                    chain.caller_file,
                    chain.suggested_gate
                ),
                None => match &gate.triggered_by {
                    Some(path) => out!("    - {} (via {})", gate.gate, path),
                    None => out!("    - {} ({})", gate.gate, gate.reason),
                },
            }
        }
    }
    out!("  next: {}", report.next_action);
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
        out!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }

    out!(
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
    out!(
        "Main:        {} ({main_relation})",
        short_commit(&report.main_head)
    );
    if let (Some(upstream_ref), Some(upstream_head)) = (&report.upstream_ref, &report.upstream_head)
    {
        let relation = upstream_relation(
            report.main_ahead_upstream_commits,
            report.main_behind_upstream_commits,
        );
        out!(
            "Upstream:    {} @ {} ({relation})",
            upstream_ref,
            short_commit(upstream_head)
        );
    }
    out!();

    if report.promoted_entries.is_empty() && report.changed_files.is_empty() {
        out!("Pending layer: none");
    } else {
        out!(
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
        out!("Promoted entries: none");
    } else {
        out!("Promoted entries:");
        for entry in report.promoted_entries.iter().take(10) {
            let label = entry
                .task
                .as_deref()
                .or(entry.branch.as_deref())
                .unwrap_or("-");
            out!(
                "  q{} session {} {} -> {}  {}",
                entry.queue_entry_id,
                entry.session_id,
                short_commit(&entry.head_commit),
                short_commit(&entry.merge_commit),
                label
            );
            if !entry.files.is_empty() {
                out!("    files: {}", capped_join(&entry.files, 5));
            }
        }
        if report.promoted_entries.len() > 10 {
            out!(
                "  and {} more promoted {}",
                report.promoted_entries.len() - 10,
                plural(report.promoted_entries.len() - 10, "entry", "entries")
            );
        }
    }

    if report.changed_files.is_empty() {
        out!("Changed files: none");
    } else {
        out!("Changed files:");
        for path in report.changed_files.iter().take(12) {
            out!("  - {path}");
        }
        if report.changed_files.len() > 12 {
            out!(
                "  and {} more {}",
                report.changed_files.len() - 12,
                plural(report.changed_files.len() - 12, "file", "files")
            );
        }
    }

    if report.conflicts.is_empty() {
        out!("Conflicts with pending layer: none");
    } else {
        out!("Conflicts with pending layer:");
        for conflict in report.conflicts.iter().take(12) {
            out!(
                "  session {}: {} (session {}, integration {})",
                conflict.session_id,
                conflict.path,
                conflict.session_path,
                conflict.promoted_path
            );
        }
        if report.conflicts.len() > 12 {
            out!(
                "  and {} more {}",
                report.conflicts.len() - 12,
                plural(report.conflicts.len() - 12, "conflict", "conflicts")
            );
        }
    }

    if let Some(reconciliation) = &report.reconciliation {
        out!(
            "Reconciliation evidence: {} landed, {} ambiguous, {} unresolved, {} unrecorded",
            reconciliation.landed_entry_count,
            reconciliation.ambiguous_entry_count,
            reconciliation.unresolved_entry_count,
            reconciliation.unrecorded_commits.len()
        );
        out!("  {}", reconciliation.explanation);
    }

    out!("Delivery state: {}", report.next_action.state.as_str());
    out!("Next action: {}", report.next_action.summary);
    for command in &report.next_action.commands {
        out!("  run: {command}");
    }
    Ok(())
}

fn render_ship_plan(report: &crate::ShipPlan, json: bool, detail: bool) -> Result<(), UsageError> {
    if json {
        out!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    out!(
        "Ship plan q{} (session {})",
        report.queue_entry.id,
        report.originating_session.id
    );
    out!(
        "Integration: {} @ {}",
        report.integration_ref,
        report.integration_sha
    );
    out!("Publication prefix: {}", report.publication_sha);
    // One line per promoted entry ever included is unbounded and grows with
    // the repository's history; the count plus the boundary entries is what a
    // reviewer checks.
    // Most of an included prefix is already on the remote; enumerating it buries
    // the handful of entries this push actually publishes (issue #141).
    let label = |entry: &crate::ship::ShipPromotedEntry| {
        format!("q{}@{}", entry.queue_entry_id, entry.promotion_sha)
    };
    let newly = report
        .included_entries
        .iter()
        .filter(|entry| entry.newly_published)
        .map(label)
        .collect::<Vec<_>>();
    let already_published = report.included_entries.len() - newly.len();
    out!(
        "Included entries: {} total, {} already on the remote default branch",
        report.included_entries.len(),
        already_published
    );
    if newly.is_empty() {
        out!("Newly published by this push: none — the remote already contains this prefix");
    } else {
        out!(
            "Newly published by this push: {} ({})",
            newly.len(),
            if detail || newly.len() <= SHIP_ENTRY_CAP {
                newly.join(", ")
            } else {
                format!(
                    "{}, ... , {} — rerun with --detail for all",
                    newly[..2].join(", "),
                    newly[newly.len() - 1]
                )
            }
        );
    }
    if detail {
        let included = report
            .included_entries
            .iter()
            .map(label)
            .collect::<Vec<_>>();
        out!("Included prefix in full: {}", included.join(", "));
    }
    if !report.excluded_entries.is_empty() {
        out!(
            "Excluded later entries: {}",
            report
                .excluded_entries
                .iter()
                .map(|entry| format!("q{}@{}", entry.queue_entry_id, entry.promotion_sha))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    out!(
        "Local default:  {} @ {}",
        report.local_default_branch_ref,
        report.local_default_branch_sha
    );
    out!(
        "Remote default: {}/{} @ {}",
        report.target.remote_name,
        report.remote_default_branch_ref,
        report.remote_default_branch_sha
    );
    out!(
        "Target: {} ({})",
        report.target.display_slug,
        report.target.normalized_host
    );
    out!("Freshness: {:?}", report.freshness.result);
    out!("Proposed push: {}", report.proposed_push.command.join(" "));
    out!(
        "Publication policy: {:?} (evidence {})",
        report.publication_policy.policy.mode,
        if report.publication_policy.satisfied {
            "satisfied"
        } else {
            "missing or stale"
        }
    );
    for evidence in &report.publication_policy.evidence {
        out!(
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
        out!("Publication remediation: {remediation}");
    }
    for line in repository_wide_publication_lines(
        &report.local_main_sync_assessment,
        &report.local_default_branch_ref,
        &report.publication_sha,
        &report.local_default_branch_sha,
    ) {
        out!("{line}");
    }
    let assessment = &report.local_main_sync_assessment;
    out!(
        "Local-main synchronization safe now: {}",
        if report.local_main_sync_safe {
            "yes"
        } else {
            "no"
        }
    );
    if !assessment.tracked_dirty_paths.is_empty() {
        out!(
            "Blocking tracked paths: {}",
            assessment.tracked_dirty_paths.join(", ")
        );
    }
    if !assessment.conflicting_untracked_paths.is_empty() {
        out!(
            "Blocking untracked collisions: {}",
            assessment.conflicting_untracked_paths.join(", ")
        );
    } else if !assessment.untracked_paths.is_empty() {
        out!(
            "Unrelated untracked paths preserved: {}",
            assessment.untracked_paths.join(", ")
        );
    }
    out!(
        "Confirm with: aethyme broker ship execute --entry {} --confirm {}",
        report.queue_entry.id,
        report.publication_sha
    );
    Ok(())
}

fn render_ship_execution(
    report: &crate::ShipExecutionReport,
    json: bool,
) -> Result<(), UsageError> {
    if json {
        out!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    out!(
        "Published {} to {}/{}.",
        report.published_sha,
        report.plan.target.remote_name,
        report.plan.remote_default_branch_ref
    );
    out!("Verified remote SHA: {}", report.verified_remote_sha);
    out!(
        "Publication authorization: {:?}",
        report.publication_authorization.kind
    );
    if let Some(digest) = &report.publication_authorization.reason_digest {
        out!("Break-glass reason SHA-256: {digest}");
    }
    out!(
        "Operations: fetch {}, push {}, verify {}",
        report.fetch_operation.id,
        report.push_operation.id,
        report.verify_operation.id
    );
    if report.local_main_sync.synchronized {
        out!(
            "Local main synchronized: {} -> {}",
            report.local_main_sync.before_sha,
            report.local_main_sync.after_sha
        );
    } else if let Some(command) = &report.local_main_sync.follow_up_command {
        out!("Local main unchanged. To synchronize it explicitly:");
        out!("  {command}");
    }
    Ok(())
}

fn render_integration_stability(
    report: &crate::IntegrationStabilityReport,
    json: bool,
) -> Result<(), UsageError> {
    if json {
        out!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }

    out!(
        "Integration: {} {} -> {}",
        report.branch,
        short_commit(&report.start_head),
        short_commit(&report.end_head)
    );
    out!(
        "Window:      {}s (observed {}ms)",
        report.requested_seconds,
        report.observed_ms
    );
    out!(
        "Result:      {}",
        if report.stable { "stable" } else { "moved" }
    );
    out!("{}", report.message);
    if report.live_sessions.is_empty() {
        out!("Live sessions: none");
    } else {
        out!("Live sessions:");
        for session in report.live_sessions.iter().take(10) {
            out!(
                "  session {} {} {} {}",
                session.id,
                session.status.as_str(),
                session.branch,
                session.task.as_deref().unwrap_or("-")
            );
        }
        if report.live_sessions.len() > 10 {
            out!(
                "  and {} more {}",
                report.live_sessions.len() - 10,
                plural(report.live_sessions.len() - 10, "session", "sessions")
            );
        }
    }
    for command in &report.commands {
        out!("run: {command}");
    }
    Ok(())
}

fn render_integration_reconcile(
    report: &crate::IntegrationReconcileReport,
    json: bool,
) -> Result<(), UsageError> {
    if json {
        out!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }

    out!("Local main:  {}", short_commit(&report.local_main));
    out!(
        "Upstream:    {} @ {}",
        report.upstream_ref,
        short_commit(&report.upstream_head)
    );
    out!(
        "Integration: {} -> {}",
        short_commit(&report.old_integration),
        short_commit(&report.new_integration)
    );
    if let Some(path) = &report.resolution_file {
        out!("Resolution:  {path}");
    }
    if let Some(digest) = &report.plan_digest {
        out!("Plan digest: {digest}");
    }
    out!(
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
        out!(
            "  q{} session {}: {} — {}",
            entry.queue_entry_id,
            entry.session_id,
            entry.classification.as_str(),
            entry.evidence
        );
        if !entry.conflicts.is_empty() {
            out!("    conflicts: {}", capped_join(&entry.conflicts, 5));
        }
    }
    if let Some(template) = &report.resolution_template {
        out!(
            "Resolution template: {} recorded, {} unrecorded ({})",
            template.document.resolutions.len(),
            template.document.unrecorded_resolutions.len(),
            if template.complete {
                "complete"
            } else {
                "operator input required"
            }
        );
        out!(
            "  recorded classification: {}",
            template
                .field_contract
                .recorded_classification_allowed_values
                .join(", ")
        );
        for rule in &template.field_contract.unrecorded_dispositions {
            out!(
                "  {}: upstream_commit {}; {}",
                rule.value,
                rule.upstream_commit,
                rule.condition
            );
        }
        out!("  operator: {}", template.field_contract.operator);
        out!("  reason: {}", template.field_contract.reason);
    }
    for warning in &report.warnings {
        out!("Warning: {warning}");
    }
    out!("Next action: {}", report.next_action);
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
    out!(
        "Advisory {}: {} [{} / {}]",
        advisory.id,
        advisory_text(&advisory.identity),
        advisory.severity.as_str(),
        advisory.resolution_state.as_str(),
    );
    out!(
        "Session: {}",
        advisory
            .session_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "none".into())
    );
    out!(
        "Queue entry: {}",
        advisory
            .queue_entry_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "none".into())
    );
    out!(
        "Integration SHA: {}",
        advisory.integration_sha.as_deref().unwrap_or("none")
    );
    out!("Created: {}", advisory.created_at);
    out!(
        "Acknowledged: {}",
        advisory
            .acknowledged_at
            .map(|time| time.to_string())
            .unwrap_or_else(|| "no".into())
    );
    out!(
        "Suppressed: {}",
        advisory
            .suppressed_at
            .map(|time| time.to_string())
            .unwrap_or_else(|| "no".into())
    );
    out!(
        "Resolved: {}",
        advisory
            .resolved_at
            .map(|time| time.to_string())
            .unwrap_or_else(|| "no".into())
    );
    if let Some(evidence) = advisory.resolution_evidence.as_deref() {
        out!("Resolution evidence: {}", advisory_text(evidence));
    }
    if !advisory.paths.is_empty() {
        out!("Paths:");
        for path in &advisory.paths {
            out!("  - {}", advisory_text(path));
        }
    }
    if !advisory.evidence.is_empty() {
        out!("Evidence:");
        for evidence in &advisory.evidence {
            out!(
                "  - {}: {}",
                advisory_text(&evidence.kind),
                advisory_text(&evidence.summary)
            );
        }
    }
    if advisory.resolution_state == crate::AdvisoryResolutionState::Outstanding {
        out!("Acknowledge: aethyme broker advisories ack {}", advisory.id);
        if advisory.audience == crate::AdvisoryAudience::Maintainer {
            out!(
                "Suppress: aethyme broker advisories suppress {}",
                advisory.id
            );
        }
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
            "watch requires `pr` followed by start, list, show, poll, tick, batches, ack, pause, resume, or stop"
                .into(),
        ));
    }
    let action = parsed
        .positional
        .get(1)
        .map(String::as_str)
        .ok_or_else(|| {
            UsageError::Message(
                "watch pr requires start, list, show, poll, tick, batches, ack, pause, resume, or stop"
                    .into(),
            )
        })?;
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    match action {
        "monitoring" => {
            let mode = parsed
                .positional
                .get(2)
                .map(String::as_str)
                .ok_or_else(|| {
                    UsageError::Message(
                        "watch pr monitoring requires activate, deactivate, or status".into(),
                    )
                })?;
            let session_id = parsed.session.ok_or_else(|| {
                UsageError::Message("watch pr monitoring requires --session <id>".into())
            })?;
            // Proves the session exists before recording a flag against it.
            broker.store().session(session_id)?;
            let root = broker.main_root().to_path_buf();
            let io = |result: std::io::Result<()>| -> Result<(), UsageError> {
                result.map_err(|error| {
                    UsageError::Message(format!("cannot record PR monitoring state: {error}"))
                })
            };
            match mode {
                "activate" => io(crate::activate_pr_monitoring(&root, session_id))?,
                "deactivate" => io(crate::deactivate_pr_monitoring(&root, session_id))?,
                "status" => {}
                other => {
                    return Err(UsageError::Message(format!(
                        "unknown watch pr monitoring mode {other:?}; expected activate, deactivate, or status"
                    )));
                }
            }
            let active = crate::pr_monitoring_is_active(&root, session_id);
            if parsed.json {
                out!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "session_id": session_id,
                        "pr_monitoring_active": active,
                    }))?
                );
            } else if active {
                out!("session {session_id}: PR monitoring active");
            } else {
                out!("session {session_id}: PR monitoring off");
            }
        }
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
                out!("{}", serde_json::to_string_pretty(&watches)?);
            } else if watches.is_empty() {
                out!("No pull request watches.");
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
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                out!(
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
        "tick" => {
            let limit = parsed
                .limit
                .map(|limit| limit as usize)
                .unwrap_or(crate::DEFAULT_PR_SCHEDULER_LIMIT);
            let report = broker.tick_pull_request_watches(
                &crate::GithubCliPullRequestWatchProvider,
                now_ms(),
                limit,
            )?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                out!(
                    "PR scheduler tick: {} due, {} polled, {} failed, {} deferred.",
                    report.due_watch_count,
                    report.successful_watch_count,
                    report.failed_watch_count,
                    report.deferred_watch_count,
                );
                if let Some(retry_at) = report.rate_limit_until {
                    out!("Provider rate limit: retry no earlier than {retry_at}.");
                }
                match report.next_tick_at {
                    Some(next) => out!("Next due tick: {next}."),
                    None => out!("Next due tick: none."),
                }
            }
        }
        "batches" => {
            let watch_id = parsed.note_id.ok_or_else(|| {
                UsageError::Message("watch pr batches requires --id <watch-id>".into())
            })?;
            let batches = broker.pull_request_activity_batches(watch_id, parsed.all)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&batches)?);
            } else if batches.is_empty() {
                out!("No pull request activity batches.");
            } else {
                for batch in batches {
                    out!(
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
                out!("{}", serde_json::to_string_pretty(&batch)?);
            } else {
                out!("Batch {} acknowledged as {}.", batch.id, outcome.as_str());
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
                "unknown watch pr action {other:?} — expected start, list, show, poll, tick, batches, ack, pause, resume, or stop"
            )));
        }
    }
    Ok(())
}

/// `representation scan|status|record` -- the lane for work that reached the
/// default branch through a provider-side merge instead of through submit.
/// Scanning is inspection; recording is the deliberate, digest-bound write that
/// lets such a session close (#152).
fn run_representation(parsed: Parsed) -> Result<(), UsageError> {
    let action = parsed
        .positional
        .first()
        .map(String::as_str)
        .unwrap_or("scan");
    let session = parsed.session.ok_or(UsageError::Message(
        "representation requires --session <id>".into(),
    ))?;
    // `record` is the only writer; scanning must not take the write lock.
    let mut broker = open_broker(action != "record")?;
    match action {
        "scan" | "status" => {
            let scan = broker.scan_session_representation(session)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&scan)?);
            } else {
                render_representation_scan(&scan);
            }
        }
        "record" => {
            let confirm = parsed.confirm.as_deref().ok_or(UsageError::Message(
                "representation record requires --confirm <sha256>".into(),
            ))?;
            let scan = broker.record_session_representation(session, confirm)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&scan)?);
            } else {
                render_representation_scan(&scan);
            }
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown representation action {other:?}; expected scan, status, or record"
            )));
        }
    }
    Ok(())
}

fn short(commit: &str) -> &str {
    &commit[..12.min(commit.len())]
}

fn render_representation_scan(scan: &crate::RepresentationScan) {
    out!(
        "Session {} head {} ({} changed path(s) since {})",
        scan.session_id,
        short(&scan.session_head),
        scan.paths(),
        short(&scan.base)
    );
    if let Some(record) = &scan.existing {
        match record.representing_commit.as_deref() {
            Some(commit) => out!(
                "  recorded: represented by {} on {} ({})",
                short(commit),
                record.representing_ref,
                record.discovery.as_str()
            ),
            None => out!(
                "  recorded: nothing to represent on {} ({})",
                record.representing_ref,
                record.discovery.as_str()
            ),
        }
        return;
    }
    match &scan.search.outcome {
        crate::LandingOutcome::NothingToRepresent => {
            out!(
                "  nothing to represent: this head adds no net change to {}",
                scan.branch
            );
            out!(
                "  next: aethyme broker representation record --session {} --confirm {}",
                scan.session_id,
                scan.digest
            );
        }
        crate::LandingOutcome::Landed(landing) => {
            out!(
                "  landed on {} as {} ({})",
                scan.branch,
                short(&landing.commit),
                landing.subject
            );
            out!(
                "  {} of {} path(s) matched; {} commit(s) examined",
                landing.paths,
                scan.paths(),
                scan.search.examined
            );
            out!(
                "  next: aethyme broker representation record --session {} --confirm {}",
                scan.session_id,
                scan.digest
            );
        }
        crate::LandingOutcome::NotFound { closest } => {
            out!(
                "  NOT represented on {} ({} commit(s) examined{})",
                scan.branch,
                scan.search.examined,
                if scan.search.truncated {
                    ", search truncated"
                } else {
                    ""
                }
            );
            match closest {
                Some(closest) => out!(
                    "  closest was {} ({}): matched {} path(s), missing {}",
                    short(&closest.commit),
                    closest.subject,
                    closest.matched_paths,
                    closest.missing_path
                ),
                None => out!("  no commit on {} carried any of this work", scan.branch),
            }
            out!(
                "  next: aethyme broker submit --session {}",
                scan.session_id
            );
        }
    }
}

fn run_reclaim(parsed: Parsed) -> Result<(), UsageError> {
    let action = parsed
        .positional
        .first()
        .map(String::as_str)
        .unwrap_or("plan");
    let mut broker = open_broker(true)?;
    // This repository's own worktree directory, not the shared container:
    // reclaiming another repository's build output from here would be a
    // surprise, and that repository's broker knows which of its sessions are
    // live while this one does not.
    let Some(root) = broker.worktree_root_plan()?.preferred_root else {
        return Err(UsageError::Message(
            "this repository has no broker worktree root to reclaim from".into(),
        ));
    };
    // Only sessions actually working are protected. An idle or stale session
    // may be one that simply cannot be closed, and those are precisely the
    // worktrees whose build output accumulates.
    let now = now_ms();
    let active: Vec<std::path::PathBuf> = broker
        .agents(now)
        .unwrap_or_default()
        .into_iter()
        .filter(|agent| agent.derived_status == crate::SessionStatus::Active)
        .map(|agent| std::path::PathBuf::from(agent.session.worktree_path.clone()))
        .collect();
    let candidates = crate::scan_reclaim(&root, &active);
    let digest = crate::reclaim::plan_digest(&root, &candidates);
    let plan = crate::ReclaimPlan {
        digest: digest.clone(),
        root: root.clone(),
        reclaimable_bytes: crate::reclaimable_bytes(&candidates),
        total_bytes: candidates.iter().map(|c| c.bytes).sum(),
        candidates,
    };
    let gib = |bytes: u64| format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0));
    match action {
        "plan" => {
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                out!(
                    "Reclaim plan {}: {} reclaimable of {} found",
                    plan.digest,
                    gib(plan.reclaimable_bytes),
                    gib(plan.total_bytes)
                );
                for candidate in plan.candidates.iter().take(20) {
                    out!(
                        "  {:>10}  {}{}",
                        gib(candidate.bytes),
                        candidate.path.display(),
                        if candidate.reclaimable {
                            ""
                        } else {
                            "  (active session; kept)"
                        }
                    );
                }
                if plan.reclaimable_bytes == 0 {
                    out!("Nothing to reclaim.");
                } else {
                    out!(
                        "Apply with: aethyme broker reclaim apply --confirm {}",
                        plan.digest
                    );
                }
            }
        }
        "apply" => {
            let confirm = parsed.confirm.as_deref().ok_or_else(|| {
                UsageError::Message("reclaim apply requires --confirm <sha256>".into())
            })?;
            // Re-derived from a fresh scan, so a plan whose candidates moved is
            // refused rather than applied to a different set than was reviewed.
            if confirm != plan.digest {
                return Err(UsageError::Message(format!(
                    "confirmation does not match the current plan; re-run `aethyme broker reclaim plan` and review it again (current {})",
                    plan.digest
                )));
            }
            let outcome = crate::apply_reclaim(&plan);
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&outcome)?);
            } else {
                out!(
                    "Reclaimed {} from {} director{}",
                    gib(outcome.reclaimed_bytes),
                    outcome.removed.len(),
                    if outcome.removed.len() == 1 {
                        "y"
                    } else {
                        "ies"
                    }
                );
                for skipped in &outcome.skipped {
                    out!("  skipped: {skipped}");
                }
            }
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown reclaim action {other:?}; expected plan or apply"
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
        "dispatch" => {
            let adapter = parsed.adapter.as_deref().unwrap_or("chau7");
            let worker = parsed.worker.as_deref().ok_or_else(|| {
                UsageError::Message("deliveries dispatch requires --worker <id>".into())
            })?;
            let seconds = parsed.seconds.unwrap_or(120);
            let claim = broker.claim_next_delivery(adapter, worker, seconds, now_ms())?;
            let Some(envelope) = claim.delivery else {
                if parsed.json {
                    out!("{}", serde_json::json!({"claimed": false}));
                } else {
                    out!("no delivery pending for adapter {adapter}");
                }
                return Ok(());
            };
            let session = broker.store().session(envelope.watch.session_id)?;
            let raw = match parsed.tabs_file.as_deref() {
                Some(path) => std::fs::read_to_string(path).map_err(|error| {
                    UsageError::Message(format!("cannot read {}: {error}", path.display()))
                })?,
                None => {
                    use std::io::Read;
                    let mut buffer = String::new();
                    std::io::stdin()
                        .read_to_string(&mut buffer)
                        .map_err(|error| {
                            UsageError::Message(format!("cannot read tabs from stdin: {error}"))
                        })?;
                    buffer
                }
            };
            let tabs: Vec<crate::Chau7Tab> = serde_json::from_str(&raw).map_err(|error| {
                UsageError::Message(format!(
                    "tab snapshot is not a Chau7 tab_list array: {error}"
                ))
            })?;
            let action = crate::dispatch_action(
                &tabs,
                &session.worktree_path,
                &session.branch,
                &envelope.prompt,
            );
            // Deferral and abandonment are terminal for this claim, so the
            // broker completes them. A send stays open: only the caller knows
            // whether the transport actually landed.
            match &action {
                crate::Chau7DispatchAction::Defer { why, .. } => {
                    broker.complete_delivery(
                        envelope.item.id,
                        worker,
                        envelope.item.generation,
                        crate::DeliveryCompletion::Retry,
                        Some("tab_not_ready"),
                        now_ms(),
                    )?;
                    if !parsed.json {
                        out!("deferred delivery {}: {why}", envelope.item.id);
                    }
                }
                crate::Chau7DispatchAction::Abandon { why } => {
                    broker.complete_delivery(
                        envelope.item.id,
                        worker,
                        envelope.item.generation,
                        crate::DeliveryCompletion::Failed,
                        Some("tab_unresolvable"),
                        now_ms(),
                    )?;
                    if !parsed.json {
                        out!("abandoned delivery {}: {why}", envelope.item.id);
                    }
                }
                crate::Chau7DispatchAction::Send { tab_id, .. } => {
                    if !parsed.json {
                        out!("send delivery {} to {tab_id}", envelope.item.id);
                        out!(
                            "  complete with: aethyme broker deliveries complete --id {} --worker {worker} --generation {} --outcome delivered",
                            envelope.item.id,
                            envelope.item.generation
                        );
                    }
                }
            }
            if parsed.json {
                out!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "claimed": true,
                        "delivery_id": envelope.item.id,
                        "generation": envelope.item.generation,
                        "session_id": envelope.watch.session_id,
                        "action": action,
                    }))?
                );
            }
        }
        "resolve-tab" => {
            let session_id = parsed.session.ok_or_else(|| {
                UsageError::Message("deliveries resolve-tab requires --session <id>".into())
            })?;
            let session = broker.store().session(session_id)?;
            let raw = match parsed.tabs_file.as_deref() {
                Some(path) => std::fs::read_to_string(path).map_err(|error| {
                    UsageError::Message(format!("cannot read {}: {error}", path.display()))
                })?,
                None => {
                    use std::io::Read;
                    let mut buffer = String::new();
                    std::io::stdin()
                        .read_to_string(&mut buffer)
                        .map_err(|error| {
                            UsageError::Message(format!("cannot read tabs from stdin: {error}"))
                        })?;
                    buffer
                }
            };
            let tabs: Vec<crate::Chau7Tab> = serde_json::from_str(&raw).map_err(|error| {
                UsageError::Message(format!(
                    "tab snapshot is not a Chau7 tab_list array: {error}"
                ))
            })?;
            let outcome =
                crate::resolve_session_tab(&tabs, &session.worktree_path, &session.branch);
            if parsed.json {
                let body = match &outcome {
                    Ok(resolution) => serde_json::json!({"resolved": resolution}),
                    Err(refusal) => serde_json::json!({"refused": refusal}),
                };
                out!("{}", serde_json::to_string_pretty(&body)?);
            } else {
                match &outcome {
                    Ok(resolution) => out!(
                        "session {} -> {} ({:?}{})",
                        session_id,
                        resolution.tab_id,
                        resolution.readiness,
                        if resolution.mcp_controlled {
                            ", mcp-controlled"
                        } else {
                            ""
                        }
                    ),
                    Err(refusal) => out!("refused: {refusal:?}"),
                }
            }
            if outcome.is_err() {
                std::process::exit(2);
            }
        }
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
                out!("{}", serde_json::to_string_pretty(&subscription)?);
            } else {
                out!(
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
                out!("{}", serde_json::to_string_pretty(&items)?);
            } else if items.is_empty() {
                out!("No delivery outbox items.");
            } else {
                for item in items {
                    out!(
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
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else if let Some(delivery) = report.delivery {
                out!(
                    "Claimed delivery {} generation {} for {}. Use --json to read its structured envelope and prompt.",
                    delivery.item.id,
                    delivery.item.generation,
                    delivery.subscription.target,
                );
            } else {
                out!("No pending delivery for adapter {adapter}.");
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
                out!("{}", serde_json::to_string_pretty(&item)?);
            } else {
                out!("Delivery {} is {}.", item.id, item.status.as_str());
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
        out!("{}", serde_json::to_string_pretty(watch)?);
    } else {
        out!(
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
    out!("Operation:      {}", operation.id);
    out!("Session:        {}", operation.session_id);
    out!("Provider:       {}", operation.provider.as_str());
    out!("Repository:     {}", operation.repository);
    out!("Scope:          {}", operation.scope);
    out!("Effect:         {}", operation.effect.as_str());
    out!("Status:         {}", operation.status.as_str());
    out!("Identity:       {}", operation.identity_provenance.as_str());
    out!("Command:        {}", operation.command_json);
    out!(
        "Host operation: {}",
        operation.host_operation_id.as_deref().unwrap_or("none")
    );
    out!("Reconciliation: {}", report.reconciliation.state.as_str());
    out!(
        "Write blocked:  {}",
        if report.reconciliation.write_blocked {
            "yes"
        } else {
            "no"
        }
    );
    out!("Automatic retry: forbidden");
    if let Some(evidence) = &report.reconciliation.evidence {
        out!("Evidence:       {evidence}");
    }
    if let Some(reason) = &report.reconciliation.operator_reason {
        out!("Operator reason: {reason}");
    }
    if let Some(recovery) = &report.reconciliation.recovery {
        out!("Inspect:        {}", recovery.inspection);
        out!("If succeeded:   {}", recovery.succeeded_command);
        out!("If failed:      {}", recovery.failed_command);
        out!("Blind retry is forbidden until reconciliation is recorded.");
    }
}

fn render_coordinated_operation(
    report: &crate::CoordinatedOperationReport,
    json: bool,
) -> Result<(), UsageError> {
    if json {
        out!("{}", serde_json::to_string_pretty(report)?);
    } else {
        if !report.stdout.is_empty() {
            print!("{}", report.stdout);
            if !report.stdout.ends_with('\n') {
                out!();
            }
        }
        if !report.stderr.is_empty() {
            eprint!("{}", report.stderr);
            if !report.stderr.ends_with('\n') {
                eprintln!();
            }
        }
        out!(
            "operation {}: {} {} on {} ({})",
            report.operation.id,
            report.operation.provider.as_str(),
            report.operation.status.as_str(),
            report.operation.repository,
            report.classification,
        );
        // The PR is linkable the moment it exists; starting the watch is left
        // to the caller because it polls the provider, and this command may
        // still be inside the repository write lock (#150, and #138 for why).
        if let Some(cleanup) = &report.post_merge_cleanup {
            out!(
                "post-merge integration cleanup: {} — {}",
                cleanup.state.as_str(),
                cleanup.explanation
            );
            if let Some(operation_id) = cleanup.fetch_operation_id {
                out!("  upstream refresh operation: {operation_id}");
            }
            if let Some(command) = &cleanup.next_action {
                out!("  next: {command}");
            }
        }
    }
    Ok(())
}

/// `broker review plan` -- evaluate the review policies against a change and
/// print what they would do.
///
/// A dry run with no exceptions: it reads git, reads `.aethyme/config.toml`,
/// and writes nothing. Every mutation it would make is printed as the exact
/// `aethyme broker gh` command that would make it, so an operator can read the
/// decision and run it themselves before ever switching the policy on.
///
/// It deliberately does not consult the provider. Review *spend* and live
/// Chau7 tabs are the two inputs it cannot get without a network, and the plan
/// says so rather than guessing: what it shows is the decision for a pull
/// request with no reviews spent yet, which is the case an operator is trying
/// to reason about when they are writing the rules.
fn run_review_plan(parsed: Parsed) -> Result<(), UsageError> {
    let broker = open_broker(true)?;
    // Policy is repository-level and lives in the main checkout; the change
    // being planned is in whatever worktree the caller is standing in. Reading
    // both from the main root would plan the main branch against itself, which
    // is an empty diff and an answer that looks perfectly correct.
    let root = broker.main_root().to_path_buf();
    let change_root = std::env::current_dir().map_err(|error| {
        UsageError::Message(format!("cannot read the working directory: {error}"))
    })?;
    let base = parsed
        .base
        .clone()
        .unwrap_or_else(|| "aethyme/integration".to_string());
    let pull_request = parsed.pr_number.unwrap_or(0);

    let paths = git_lines(
        &change_root,
        &["diff", "--name-only", &format!("{base}...HEAD")],
    )?;
    let messages = git_output(
        &change_root,
        &["log", "--format=%B%x00", &format!("{base}..HEAD")],
    )?;
    let classification = crate::CommitClassification::merge(
        messages
            .split('\0')
            .filter(|m| !m.trim().is_empty())
            .map(crate::parse_classification),
    );

    let trigger = crate::ReviewTriggerPolicy::load(&root).map_err(to_usage)?;
    let routing = crate::ReviewRoutingPolicy::load(&root).map_err(to_usage)?;
    let projection_policy = crate::PrProjectionPolicy::load(&root).map_err(to_usage)?;

    // `review plan` is the offline preview: git and config, no provider call,
    // runnable while other sessions work. It therefore cannot know which
    // lifecycle transition this is, and says so in `assumptions` rather than
    // presenting a guess as a reading. `review run` derives all of this for
    // real.
    let facts = crate::ChangeFacts {
        trigger: Some(crate::ReviewTrigger::PullRequestOpened),
        paths: paths.clone(),
        authored_by_model: classification.model.clone(),
        classification: classification.clone(),
        from_fork: false,
        first_time_contributor: false,
    };
    let eligible = crate::eligible_types(&trigger, &facts);
    let head = git_output(&change_root, &["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    let decisions = crate::schedule(
        &trigger,
        &eligible,
        &std::collections::BTreeMap::new(),
        &head,
        now_ms(),
    );

    let dispatch: Vec<crate::ReviewDispatchAction> = decisions
        .iter()
        .filter_map(|decision| match decision {
            crate::ReviewTriggerDecision::Request { review_type, .. } => Some(
                crate::dispatch_review(&routing, &root, review_type, pull_request, &head, &[], &[]),
            ),
            _ => None,
        })
        .collect();

    let reviews: Vec<crate::ProjectedReview> = decisions
        .iter()
        .map(|decision| crate::ProjectedReview {
            review_type: decision.review_type().to_string(),
            state: match decision {
                crate::ReviewTriggerDecision::Request { .. } => {
                    crate::ProjectedReviewState::Requested
                }
                crate::ReviewTriggerDecision::Defer { .. } => crate::ProjectedReviewState::Deferred,
                crate::ReviewTriggerDecision::Skip { .. } => crate::ProjectedReviewState::Skipped,
            },
            detail: None,
        })
        .collect();
    let projection = crate::ReviewProjection {
        head: Some(head.clone()),
        reviews,
        classification: classification.clone(),
        conflicts: Vec::new(),
    };
    let projection_actions = crate::project(
        &projection_policy,
        &projection,
        &crate::PrProjectionFacts {
            pull_request,
            ..Default::default()
        },
    );

    let report = serde_json::json!({
        "policy_root": root.display().to_string(),
        "change_root": change_root.display().to_string(),
        "base": base,
        "head": head,
        "pull_request": pull_request,
        "changed_paths": paths.len(),
        "classification": classification,
        "trigger_enabled": trigger.enabled,
        "routing_enabled": routing.enabled,
        "projection_enabled": projection_policy.enabled,
        "eligible": eligible,
        "decisions": decisions,
        "dispatch": dispatch,
        "projection": projection_actions,
        "assumptions": [
            "no reviews have been spent on this pull request yet",
            "no Chau7 tabs and no reviews are in flight",
            "the pull request carries no labels and no Aethyme comment",
            "the change is a newly opened pull request, by a known contributor, \
             not from a fork",
        ],
        "performed": false,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|e| UsageError::Message(e.to_string()))?
    );
    Ok(())
}

/// Read what the provider says about a pull request right now, read-only.
///
/// `None` means the question could not be answered -- no `gh`, no auth, no such
/// pull request. Callers treat that as "no observation", which makes the tick
/// derive `PullRequestOpened` and re-ask rather than invent a transition. An
/// unnecessary review is the documented cost of an unverified signal; a
/// silently skipped one is not.
fn read_pull_request_snapshot(
    root: &Path,
    repository: &str,
    pull_request: i64,
) -> Option<crate::ProviderPullRequest> {
    let output = std::process::Command::new("gh")
        .current_dir(root)
        .args([
            "pr",
            "view",
            &pull_request.to_string(),
            "--repo",
            repository,
            "--json",
            "headRefOid,baseRefName,isDraft,state,isCrossRepository,reviews",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    // Dismissal is an event, and a snapshot can only show it as a count that
    // grew since the last look.
    let dismissed_reviews = json["reviews"]
        .as_array()
        .map(|reviews| {
            reviews
                .iter()
                .filter(|review| {
                    review["state"]
                        .as_str()
                        .is_some_and(|state| state.eq_ignore_ascii_case("dismissed"))
                })
                .count() as i64
        })
        .unwrap_or(0);
    Some(crate::ProviderPullRequest {
        head_commit: json["headRefOid"].as_str().unwrap_or_default().to_string(),
        base_ref: json["baseRefName"].as_str().unwrap_or_default().to_string(),
        is_draft: json["isDraft"].as_bool().unwrap_or(false),
        state: json["state"]
            .as_str()
            .unwrap_or("open")
            .to_ascii_lowercase(),
        from_fork: json["isCrossRepository"].as_bool().unwrap_or(false),
        dismissed_reviews,
        author_association: read_author_association(root, repository, pull_request),
    })
}

/// The author's relationship to the repository, from the provider.
///
/// A separate call because `gh pr view --json` does not expose
/// `authorAssociation`; the REST representation does. Read-only, and a failure
/// answers `None`, which [`crate::first_time_contributor`] treats as "not new"
/// rather than as "new".
fn read_author_association(root: &Path, repository: &str, pull_request: i64) -> Option<String> {
    let output = std::process::Command::new("gh")
        .current_dir(root)
        .args([
            "api",
            &format!("repos/{repository}/pulls/{pull_request}"),
            "--jq",
            ".author_association",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() || value == "null" {
        return None;
    }
    Some(value.to_ascii_lowercase())
}

/// The change itself -- paths, declarations, head -- from the provider.
///
/// `review run` normally describes the working directory it was invoked in,
/// which is right for an agent reviewing its own branch and impossible for a
/// sweep: a tick visits every open pull request and is standing in none of
/// them. Asking the provider needs no fetch, no checkout, and no local ref, so
/// one tick can route a repository it has never cloned.
fn read_change_from_provider(
    root: &Path,
    repository: &str,
    pull_request: i64,
) -> Option<(Vec<String>, crate::CommitClassification, String)> {
    let output = std::process::Command::new("gh")
        .current_dir(root)
        .args([
            "pr",
            "view",
            &pull_request.to_string(),
            "--repo",
            repository,
            "--json",
            "files,commits,headRefOid",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let paths = json["files"]
        .as_array()
        .map(|files| {
            files
                .iter()
                .filter_map(|file| file["path"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    // Trailers live in the body, and `merge` is oldest-first, so the order the
    // provider returns commits in is the order declarations must be read in.
    let classification = crate::CommitClassification::merge(
        json["commits"]
            .as_array()
            .map(|commits| {
                commits
                    .iter()
                    .map(|commit| {
                        let headline = commit["messageHeadline"].as_str().unwrap_or_default();
                        let body = commit["messageBody"].as_str().unwrap_or_default();
                        crate::parse_classification(&format!("{headline}\n{body}"))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    );
    let head = json["headRefOid"].as_str()?.to_string();
    Some((paths, classification, head))
}

/// Whether `head` has `previous` in its history.
///
/// This is what separates a commit added on top from a head that replaced the
/// old one, and only a repository can answer it. An unknown answer is `false`,
/// which reports `ReplacementCommit`: treating a rewrite as an append would
/// tell a rule that history it already reviewed is still intact when it may not
/// be, and that is the direction that loses a review.
fn head_descends_from(root: &Path, previous: &str, head: &str) -> bool {
    if previous.is_empty() || head.is_empty() {
        return false;
    }
    crate::git::git_command()
        .current_dir(root)
        .args(["merge-base", "--is-ancestor", previous, head])
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Everything the decision plane reads about a change, gathered for real.
///
/// The trigger and the provider-supplied facts used to be hardcoded here, which
/// meant every rule keyed on `on`, `from_fork` or `first_time_contributor`
/// parsed, loaded, and never matched. Returns the snapshot alongside the facts
/// so the caller can record the observation once the tick has acted on it.
fn gather_change_facts(
    store_root: &Path,
    repository: &str,
    pull_request: i64,
    paths: Vec<String>,
    classification: crate::CommitClassification,
    previous: Option<&crate::PullRequestObservation>,
) -> (crate::ChangeFacts, Option<crate::ProviderPullRequest>) {
    let snapshot = read_pull_request_snapshot(store_root, repository, pull_request);
    let Some(snapshot) = snapshot else {
        return (
            crate::ChangeFacts {
                trigger: Some(crate::ReviewTrigger::PullRequestOpened),
                authored_by_model: classification.model.clone(),
                paths,
                classification,
                from_fork: false,
                first_time_contributor: false,
            },
            None,
        );
    };
    let descends = previous.is_some_and(|previous| {
        head_descends_from(store_root, &previous.head_commit, &snapshot.head_commit)
    });
    let facts = crate::ChangeFacts {
        trigger: Some(crate::derive_trigger(previous, &snapshot, descends)),
        paths,
        authored_by_model: classification.model.clone(),
        classification,
        from_fork: snapshot.from_fork,
        first_time_contributor: crate::first_time_contributor(
            snapshot.author_association.as_deref(),
        ),
    };
    (facts, Some(snapshot))
}

/// Read the pull request facts the projection needs, with read-only `gh`.
///
/// Read-only GitHub inspection runs directly; only writes go through the
/// coordinated lane. A repository whose `gh` is unauthenticated, or a pull
/// request that does not exist, is not a hard failure here: the projection then
/// sees an empty pull request and plans to create its comment and labels, which
/// the coordinated write refuses loudly if it was wrong. Guessing quietly is
/// what this avoids.
fn read_pull_request_facts(
    root: &Path,
    repository: &str,
    pull_request: i64,
) -> crate::PrProjectionFacts {
    let mut facts = crate::PrProjectionFacts {
        pull_request,
        ..Default::default()
    };
    let view = std::process::Command::new("gh")
        .current_dir(root)
        .args([
            "pr",
            "view",
            &pull_request.to_string(),
            "--repo",
            repository,
            "--json",
            "labels,comments",
        ])
        .output();
    if let Ok(output) = view
        && output.status.success()
        && let Ok(json) = serde_json::from_slice::<serde_json::Value>(&output.stdout)
    {
        facts.current_labels = json["labels"]
            .as_array()
            .map(|labels| {
                labels
                    .iter()
                    .filter_map(|label| label["name"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let comments: Vec<(i64, String)> = json["comments"]
            .as_array()
            .map(|comments| {
                comments
                    .iter()
                    .filter_map(|comment| {
                        Some((
                            comment["id"].as_i64()?,
                            comment["body"].as_str()?.to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        facts.owned_comment =
            crate::find_owned_comment(comments.iter().map(|(id, body)| (*id, body.as_str())));
    }
    let labels = std::process::Command::new("gh")
        .current_dir(root)
        .args([
            "label", "list", "--repo", repository, "--limit", "200", "--json", "name",
        ])
        .output();
    if let Ok(output) = labels
        && output.status.success()
        && let Ok(json) = serde_json::from_slice::<serde_json::Value>(&output.stdout)
        && let Some(array) = json.as_array()
    {
        facts.repository_labels = array
            .iter()
            .filter_map(|label| label["name"].as_str().map(String::from))
            .collect();
    }
    facts
}

/// Perform one tick of the review router for one pull request.
///
/// This is `review plan` with the assumptions replaced by facts -- the ledger
/// for what has been spent, a Chau7 snapshot for what is in flight, and a
/// read-only `gh` for what the pull request already says -- followed by the
/// effects. `--dry-run` stops after the plan, which is what makes the first
/// run against a real repository safe to look at.
fn run_review_run(parsed: Parsed) -> Result<serde_json::Value, UsageError> {
    // A dry run performs nothing, so it needs neither a session nor a write
    // lock -- which is what makes it runnable while other sessions are working.
    let session_id = match parsed.session {
        Some(id) => Some(id),
        None if parsed.dry_run => None,
        None => {
            return Err(UsageError::Message(
                "review run requires --session <id>".into(),
            ));
        }
    };
    let repository = parsed
        .repository
        .clone()
        .ok_or_else(|| UsageError::Message("review run requires --repo <owner/name>".into()))?;
    let pull_request = parsed
        .pr_number
        .ok_or_else(|| UsageError::Message("review run requires --pr <number>".into()))?;

    let mut broker = open_broker(parsed.dry_run)?;
    let root = broker.main_root().to_path_buf();
    let change_root = std::env::current_dir().map_err(|error| {
        UsageError::Message(format!("cannot read the working directory: {error}"))
    })?;
    let base = parsed
        .base
        .clone()
        .unwrap_or_else(|| "aethyme/integration".to_string());

    let trigger = crate::ReviewTriggerPolicy::load(&root).map_err(to_usage)?;
    let routing = crate::ReviewRoutingPolicy::load(&root).map_err(to_usage)?;
    let projection_policy = crate::PrProjectionPolicy::load(&root).map_err(to_usage)?;

    // Where the change is read from. The working directory is right for an
    // agent routing its own branch and impossible for a sweep, which visits
    // every open pull request and is standing in none of them; the git reads
    // below would fail on the first one. So the source is chosen before either
    // is attempted, never after.
    let (paths, classification, head) = if parsed.from_provider {
        read_change_from_provider(&root, &repository, pull_request).ok_or_else(|| {
            UsageError::Message(format!(
                "cannot read pull request {pull_request} from {repository}; \
                 --from-provider needs an authenticated gh"
            ))
        })?
    } else {
        let paths = git_lines(
            &change_root,
            &["diff", "--name-only", &format!("{base}...HEAD")],
        )?;
        let messages = git_output(
            &change_root,
            &["log", "--format=%B%x00", &format!("{base}..HEAD")],
        )?;
        let classification = crate::CommitClassification::merge(
            messages
                .split('\0')
                .filter(|m| !m.trim().is_empty())
                .map(crate::parse_classification),
        );
        let head = git_output(&change_root, &["rev-parse", "HEAD"])?
            .trim()
            .to_string();
        (paths, classification, head)
    };

    // Before reading anything, stop waiting on reviews nobody is coming back
    // for. This has to happen first: an expired row becomes `abandoned`, which
    // is neither spend nor in flight, so a tick that read either before
    // expiring would plan against a repository whose slots are still held by
    // reviewers that died days ago.
    let open = broker
        .store()
        .review_requests_in_flight(&repository)
        .map_err(to_usage)?;
    let expired = crate::expired(&routing, &open, now_ms());
    if !parsed.dry_run {
        for review in &expired {
            broker
                .store()
                .set_review_request_state(
                    review.id,
                    crate::ReviewRequestState::Abandoned,
                    Some(&review.why),
                    now_ms(),
                )
                .map_err(to_usage)?;
        }
    }
    let expired_ids: std::collections::BTreeSet<i64> =
        expired.iter().map(|review| review.id).collect();

    // The three facts `review plan` has to assume. Spend is this pull request's
    // history; concurrency is the whole repository's, because `max_concurrent`
    // is a per-repository budget. Reading the slot count from this pull
    // request's rows would quietly multiply the cap by the number of open pull
    // requests, which is the opposite of what a cap is for.
    let recorded = broker
        .store()
        .review_requests_for_pr(&repository, pull_request)
        .map_err(to_usage)?;
    let spend = crate::spend_by_type(
        &recorded
            .iter()
            .filter(|row| !expired_ids.contains(&row.id))
            .cloned()
            .collect::<Vec<_>>(),
    );
    // A dry run performs nothing, so the rows it just decided to expire are
    // still open in the database. Dropping them here is what makes the plan it
    // prints the plan a real run would follow.
    let still_open: Vec<crate::ReviewRequest> = open
        .into_iter()
        .filter(|row| !expired_ids.contains(&row.id))
        .collect();
    let in_flight = crate::in_flight(&still_open);
    let tabs = read_tab_snapshot(&parsed)?;
    let pr_facts = read_pull_request_facts(&change_root, &repository, pull_request);

    let previous = broker
        .store()
        .pull_request_observation(&repository, pull_request)
        .map_err(to_usage)?;
    let (facts, snapshot) = gather_change_facts(
        &root,
        &repository,
        pull_request,
        paths.clone(),
        classification.clone(),
        previous.as_ref(),
    );
    let eligible = crate::eligible_types(&trigger, &facts);
    let decisions = crate::schedule(&trigger, &eligible, &spend, &head, now_ms());
    let dispatch: Vec<crate::ReviewDispatchAction> = decisions
        .iter()
        .filter_map(|decision| match decision {
            crate::ReviewTriggerDecision::Request { review_type, .. } => {
                Some(crate::dispatch_review(
                    &routing,
                    &root,
                    review_type,
                    pull_request,
                    &head,
                    &tabs,
                    &in_flight,
                ))
            }
            _ => None,
        })
        .collect();

    let reviews: Vec<crate::ProjectedReview> = decisions
        .iter()
        .map(|decision| crate::ProjectedReview {
            review_type: decision.review_type().to_string(),
            state: match decision {
                crate::ReviewTriggerDecision::Request { .. } => {
                    crate::ProjectedReviewState::Requested
                }
                crate::ReviewTriggerDecision::Defer { .. } => crate::ProjectedReviewState::Deferred,
                crate::ReviewTriggerDecision::Skip { .. } => crate::ProjectedReviewState::Skipped,
            },
            detail: None,
        })
        .collect();
    let projection_actions = crate::project(
        &projection_policy,
        &crate::ReviewProjection {
            head: Some(head.clone()),
            reviews,
            classification: classification.clone(),
            conflicts: Vec::new(),
        },
        &pr_facts,
    );

    let plan = crate::plan_execution(&dispatch, &projection_actions, pull_request);

    if parsed.dry_run {
        return Ok(build_review_run_report(
            &base,
            &head,
            pull_request,
            &plan,
            &[],
            &[],
            &expired,
            facts.trigger,
            false,
        ));
    }

    // Record before performing. See `review_execution`'s module comment: a
    // crash after this point costs a missed review, and a crash before it would
    // cost a duplicated one.
    let mut recorded_now = Vec::new();
    let mut skipped = Vec::new();
    // Which row answers for which dimension, so a `gh` call that fails can
    // reopen exactly the review it failed to ask for.
    let mut rows_by_type: std::collections::BTreeMap<String, i64> =
        std::collections::BTreeMap::new();
    for write in &plan.ledger {
        let (request, created) = broker
            .store()
            .record_review_request(
                &repository,
                pull_request,
                &write.review_type,
                &head,
                write.backend,
                now_ms(),
            )
            .map_err(to_usage)?;
        if !created {
            skipped.push(serde_json::json!({
                "review_type": write.review_type,
                "why": "already recorded for this head",
                "state": request.state,
            }));
            continue;
        }
        if write.state != crate::ReviewRequestState::Requested {
            broker
                .store()
                .set_review_request_state(
                    request.id,
                    write.state,
                    write.detail.as_deref(),
                    now_ms(),
                )
                .map_err(to_usage)?;
        }
        rows_by_type.insert(write.review_type.clone(), request.id);
        recorded_now.push(write.review_type.clone());
    }

    // Every GitHub write goes through the coordinated lane, which takes the
    // repository write lock and records the operation. A failed call stops the
    // tick: the rest of the projection describes a pull request state this one
    // was supposed to establish.
    //
    // Before it stops, the review that call was asking for goes back to
    // `abandoned`. The ledger's unique index makes a row permanent for its
    // head, so leaving it at `requested` would mean one failed `gh` call
    // settles that dimension forever -- the next tick would read the row,
    // count it as spend, and skip. `abandoned` is the one state the router may
    // ask about again, which is what turns a transient GitHub failure into a
    // retry instead of a silently missing review.
    let mut performed = Vec::new();
    for call in &plan.gh {
        let report = broker
            .run_coordinated_operation(crate::CoordinatedCommand {
                session_id: session_id.expect("a non-dry run requires a session"),
                provider: crate::OperationProvider::Github,
                repository: Some(repository.clone()),
                resolved_target: None,
                scope: Some(format!("pr/{pull_request}")),
                declared_effect: Some(crate::OperationEffect::Write),
                destructive_confirmed: false,
                authorization_reason: Some(call.purpose.clone()),
                args: call.args.clone(),
            })
            .map_err(to_usage)?;
        performed.push(serde_json::json!({
            "purpose": call.purpose,
            "operation_id": report.operation.id,
            "success": report.command_success,
        }));
        if !report.command_success {
            if let Some(id) = call
                .review_type
                .as_deref()
                .and_then(|review_type| rows_by_type.get(review_type))
            {
                broker
                    .store()
                    .set_review_request_state(
                        *id,
                        crate::ReviewRequestState::Abandoned,
                        Some(&format!(
                            "the coordinated GitHub write failed: {}",
                            call.purpose
                        )),
                        now_ms(),
                    )
                    .map_err(to_usage)?;
            }
            // The report still goes out: it names the operation that failed
            // and the row that went back to `abandoned`, which is what a
            // caller needs to decide whether to retry.
            print_json(&build_review_run_report(
                &base,
                &head,
                pull_request,
                &plan,
                &performed,
                &skipped,
                &expired,
                facts.trigger,
                true,
            ))?;
            return Err(UsageError::Message(format!(
                "coordinated GitHub write failed: {}",
                call.purpose
            )));
        }
    }

    // The tick acted, so this look becomes the one the next tick compares
    // against. Deliberately last: recording it earlier would mean a tick that
    // failed part-way had already declared the transition handled, and the
    // next tick would see `Scheduled` and never retry what it missed.
    if let Some(snapshot) = snapshot.as_ref() {
        broker
            .store()
            .record_pull_request_observation(&crate::PullRequestObservation {
                repository: repository.clone(),
                pr_number: pull_request,
                head_commit: snapshot.head_commit.clone(),
                base_ref: snapshot.base_ref.clone(),
                is_draft: snapshot.is_draft,
                state: snapshot.state.clone(),
                dismissed_reviews: snapshot.dismissed_reviews,
                observed_at: now_ms(),
            })
            .map_err(to_usage)?;
    }

    Ok(build_review_run_report(
        &base,
        &head,
        pull_request,
        &plan,
        &performed,
        &skipped,
        &expired,
        facts.trigger,
        true,
    ))
}

/// Route every open pull request in a repository, once.
///
/// One bounded foreground pass, in the same shape as `watch pr tick` and for
/// the same reason: **the broker never starts a background poller.** A daemon
/// inside a coordination tool is a second thing to supervise, it holds the
/// machine-wide database open for its whole life, and it fails silently by
/// construction because nobody is watching the thing that watches. A command
/// that does one pass and exits can be run by cron, by a CI schedule, by a
/// hook, or by a person, and each of those already has a way to tell you it
/// stopped.
///
/// The bound matters as much as the pass. `--limit` caps how many pull requests
/// one invocation touches, so a repository with sixty open pull requests costs
/// a predictable number of provider calls rather than however many there happen
/// to be.
///
/// A pull request that fails is recorded and skipped, not fatal: one
/// unreachable pull request must not stop the other fifty-nine from being
/// routed.
fn run_review_tick(parsed: Parsed) -> Result<(), UsageError> {
    let repository = parsed
        .repository
        .clone()
        .ok_or_else(|| UsageError::Message("review tick requires --repo <owner/name>".into()))?;
    if parsed.session.is_none() && !parsed.dry_run {
        return Err(UsageError::Message(
            "review tick requires --session <id>, or --dry-run to plan only".into(),
        ));
    }
    let limit = parsed.limit.unwrap_or(20).clamp(1, 100);

    let root = {
        let broker = open_broker(true)?;
        broker.main_root().to_path_buf()
    };
    let open = list_open_pull_requests(&root, &repository, limit)?;

    let mut visited = Vec::new();
    for pull_request in &open {
        let mut one = parsed.clone();
        one.pr_number = Some(*pull_request);
        // A sweep stands in no pull request's checkout, so it always reads the
        // change from the provider.
        one.from_provider = true;
        match run_review_run(one) {
            Ok(report) => visited.push(serde_json::json!({
                "pull_request": pull_request,
                "trigger": report.get("trigger").cloned(),
                // The head every handoff below was decided against. An adapter
                // needs it to check out the right commit and to close the row
                // it was actually handed, rather than whatever the most recent
                // push made current in between.
                "head": report.get("head").cloned(),
                "requested": report
                    .get("plan")
                    .and_then(|plan| plan.get("ledger"))
                    .cloned(),
                // Carried whole, not counted: a sweep exists so that one
                // adapter invocation can start every review it produced, and a
                // count would force the adapter to re-run `review run` per
                // pull request to learn what it was handed.
                "chau7_handoff": report.get("chau7_handoff").cloned(),
                "expired": report.get("expired").cloned(),
                "ok": true,
            })),
            // A pull request that cannot be routed is reported and left
            // behind. Stopping here would let one unreachable pull request
            // decide that none of the others get reviewed.
            Err(error) => visited.push(serde_json::json!({
                "pull_request": pull_request,
                "ok": false,
                "error": match error {
                    UsageError::Message(message) => message,
                    UsageError::Exit { message, .. } => message,
                    UsageError::Help => "usage".to_string(),
                    UsageError::SilentExit(code) => format!("exited with code {code}"),
                },
            })),
        }
    }

    print_json(&serde_json::json!({
        "repository": repository,
        "limit": limit,
        "open_pull_requests": open.len(),
        "performed": !parsed.dry_run,
        "visited": visited,
    }))
}

/// Open pull request numbers, oldest first, capped.
///
/// Oldest first so a repository with more open pull requests than `--limit`
/// makes progress on a fixed set rather than re-routing whatever happens to be
/// newest every pass and never reaching the rest.
fn list_open_pull_requests(
    root: &Path,
    repository: &str,
    limit: u32,
) -> Result<Vec<i64>, UsageError> {
    let output = std::process::Command::new("gh")
        .current_dir(root)
        .args([
            "pr",
            "list",
            "--repo",
            repository,
            "--state",
            "open",
            "--limit",
            &limit.to_string(),
            "--json",
            "number",
        ])
        .output()
        .map_err(|error| UsageError::Message(format!("gh pr list: {error}")))?;
    if !output.status.success() {
        return Err(UsageError::Message(format!(
            "cannot list open pull requests in {repository}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let json: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| UsageError::Message(format!("gh pr list returned no JSON: {error}")))?;
    let mut numbers: Vec<i64> = json
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row["number"].as_i64())
                .collect()
        })
        .unwrap_or_default();
    numbers.sort_unstable();
    Ok(numbers)
}

/// Print one JSON value, pretty.
fn print_json(value: &serde_json::Value) -> Result<(), UsageError> {
    out!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|e| UsageError::Message(e.to_string()))?
    );
    Ok(())
}

/// The tab snapshot, or none.
///
/// An absent snapshot is not an error: it means "no tabs", and routing then
/// defers every Chau7 review rather than spawning into a workspace it cannot
/// see. That is the safe reading, and it is what makes `review run` usable from
/// a cron that has no Chau7 access at all -- it still records, mentions bots,
/// and projects.
fn read_tab_snapshot(parsed: &Parsed) -> Result<Vec<crate::Chau7Tab>, UsageError> {
    let Some(path) = parsed.tabs_file.as_deref() else {
        return Ok(Vec::new());
    };
    let raw = std::fs::read_to_string(path)
        .map_err(|error| UsageError::Message(format!("cannot read {}: {error}", path.display())))?;
    serde_json::from_str(&raw).map_err(|error| {
        UsageError::Message(format!(
            "tab snapshot is not a Chau7 tab_list array: {error}"
        ))
    })
}

fn build_review_run_report(
    base: &str,
    head: &str,
    pull_request: i64,
    plan: &crate::ReviewExecutionPlan,
    performed: &[serde_json::Value],
    skipped: &[serde_json::Value],
    expired: &[crate::ExpiredReview],
    trigger: Option<crate::ReviewTrigger>,
    executed: bool,
) -> serde_json::Value {
    serde_json::json!({
        "base": base,
        "head": head,
        "pull_request": pull_request,
        // Which transition this tick decided had happened. Absent when the
        // provider could not be reached, which is itself worth seeing: the
        // whole rule set then ran against a change nobody could describe.
        "trigger": trigger,
        "plan": plan,
        "performed": executed,
        "github_operations": performed,
        "already_recorded": skipped,
        // Reviews this tick stopped waiting for, and therefore may re-ask. An
        // entry here every tick means something starts reviews and never
        // reports back, which is worth more attention than the retry it causes.
        "expired": expired,
        // The one thing the broker cannot do itself. An adapter with Chau7
        // access starts these, then closes each row with
        // `aethyme broker review state --repo <r> --pr <n> --type <t> --state <s>`.
        "chau7_handoff": plan.chau7,
    })
}

fn to_usage<E: std::fmt::Display>(error: E) -> UsageError {
    UsageError::Message(error.to_string())
}

fn git_output(root: &Path, args: &[&str]) -> Result<String, UsageError> {
    let output = crate::git::git_command()
        .current_dir(root)
        .args(args)
        .output()
        .map_err(|error| UsageError::Message(format!("git {}: {error}", args.join(" "))))?;
    if !output.status.success() {
        return Err(UsageError::Message(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn git_lines(root: &Path, args: &[&str]) -> Result<Vec<String>, UsageError> {
    Ok(git_output(root, args)?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(String::from)
        .collect())
}

/// Print the review ledger for a repository, or for one pull request in it.
///
/// The executor writes this table and nothing reads it back, which is the
/// difference between a review that is missing and a review that is missing
/// silently. A row carries who was asked, for which head, and how it ended, so
/// "why was there no security review on #412" is answered by one command
/// rather than by reading the router's source.
fn run_review_ledger(parsed: Parsed) -> Result<(), UsageError> {
    let repository = parsed
        .repository
        .clone()
        .ok_or_else(|| UsageError::Message("review ledger requires --repo <owner/name>".into()))?;
    let mut broker = open_broker(true)?;
    let rows = match parsed.pr_number {
        Some(pull_request) => broker
            .store()
            .review_requests_for_pr(&repository, pull_request),
        None => broker.store().review_requests_for_repository(&repository),
    }
    .map_err(to_usage)?;

    if parsed.json {
        out!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        out!("No reviews recorded for {repository}.");
        return Ok(());
    }
    for row in &rows {
        let head = &row.head_commit[..12.min(row.head_commit.len())];
        out!(
            "#{:<5} {:<10} {:<10} {:<16} {head}",
            row.pr_number,
            row.review_type,
            row.state.label(),
            row.backend
        );
        if let Some(detail) = &row.detail {
            out!("        {detail}");
        }
    }
    Ok(())
}

/// Report what became of one requested review.
///
/// This is the other half of `review run`'s handoff. The broker decides and
/// records; a Chau7 adapter or a provider bot performs, and closes the row
/// here. Without it the ledger only ever says `requested`, and the router's
/// concurrency slots fill up and never drain.
///
/// `--head` is optional because a reviewer reporting now was almost certainly
/// asked most recently; naming a head is how a late report about a superseded
/// commit lands on the right row instead of the current one.
fn run_review_state(parsed: Parsed) -> Result<(), UsageError> {
    let repository = parsed.repository.clone().ok_or_else(|| {
        UsageError::Message(
            "review state requires --repo <owner/name> --pr <number> --type <review-type> --state <state>"
                .into(),
        )
    })?;
    let pull_request = parsed
        .pr_number
        .ok_or_else(|| UsageError::Message("review state requires --pr <number>".into()))?;
    let review_type = parsed
        .review_type
        .clone()
        .ok_or_else(|| UsageError::Message("review state requires --type <review-type>".into()))?;
    let label = parsed
        .review_state
        .clone()
        .ok_or_else(|| UsageError::Message("review state requires --state <state>".into()))?;
    let state = crate::ReviewRequestState::parse(&label).ok_or_else(|| {
        UsageError::Message(format!(
            "unknown review state {label:?}; expected requested, running, satisfied, failed, recorded, or abandoned"
        ))
    })?;

    let mut broker = open_broker(parsed.read_only_snapshot)?;
    let existing = broker
        .store()
        .latest_review_request(
            &repository,
            pull_request,
            &review_type,
            parsed.head.as_deref(),
        )
        .map_err(to_usage)?
        // Refusing is the point: a report about a review nobody requested
        // means the reporter and the router disagree about what was asked
        // for, and inventing a row here would bury that disagreement.
        .ok_or_else(|| {
            UsageError::Message(format!(
                "no {review_type} review is recorded for {repository}#{pull_request}{}; `aethyme broker review ledger --repo {repository} --pr {pull_request}` lists what is",
                match parsed.head.as_deref() {
                    Some(head) => format!(" at {head}"),
                    None => String::new(),
                }
            ))
        })?;
    let updated = broker
        .store()
        .set_review_request_state(existing.id, state, parsed.note.as_deref(), now_ms())
        .map_err(to_usage)?;

    if parsed.json {
        out!("{}", serde_json::to_string_pretty(&updated)?);
    } else {
        out!(
            "{} review on {}#{} at {} is now {}.",
            updated.review_type,
            updated.repository,
            updated.pr_number,
            &updated.head_commit[..12.min(updated.head_commit.len())],
            updated.state.label()
        );
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
                "review requires plan, run, ledger, state, register, show, request, unlock, reassign, or abandon"
                    .into(),
            )
        })?;
    if parsed.positional.len() != 1 {
        return Err(UsageError::Message(format!(
            "review {action} accepts no positional arguments"
        )));
    }
    // `plan` is the only review action that is about a change rather than a
    // session: it answers "what would this repository do about this diff", and
    // it performs nothing, so it neither needs nor should require a session.
    if action == "plan" {
        return run_review_plan(parsed);
    }
    if action == "run" {
        let report = run_review_run(parsed)?;
        return print_json(&report);
    }
    // A sweep over every open pull request, which is `run` repeated. It takes
    // its own `--session` check rather than the shared one below because
    // `--dry-run` must stay usable without one, exactly as it is for `run`.
    if action == "tick" {
        return run_review_tick(parsed);
    }
    // `ledger` and `state` are about the router's ledger rather than a
    // session's review lifecycle, so they take a repository instead of the
    // `--session` every action below requires. `ledger` reads and `state` is
    // written by whoever performed the review, which is never this broker.
    if action == "ledger" {
        return run_review_ledger(parsed);
    }
    if action == "state" {
        return run_review_state(parsed);
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
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                out!(
                    "Review lifecycle {} abandoned; {}",
                    report.lifecycle.id,
                    report.next_action
                );
            }
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown review action {other:?}; expected plan, run, ledger, state, register, show, request, unlock, reassign, or abandon"
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
        out!("{}", serde_json::to_string_pretty(report)?);
    } else {
        out!(
            "Review lifecycle {}: {}{}",
            report.lifecycle.id,
            report.lifecycle.state.as_str(),
            if report.changed { " (advanced)" } else { "" }
        );
        out!(
            "  session/queue: {} / {}",
            report.lifecycle.session_id,
            report
                .lifecycle
                .queue_entry_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "not yet verified".into())
        );
        out!(
            "  repository/PR: {} / #{}",
            report.lifecycle.repository,
            report.lifecycle.pr_number
        );
        out!("  commit: {}", report.lifecycle.commit_sha);
        if let Some(operation_id) = report.operation_id {
            out!("  coordinated operation: {operation_id}");
        }
        out!("  next: {}", report.next_action);
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
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                out!(
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
                    out!("  owner session: {session_id}");
                }
                if let Some(remediation) = report.remediation {
                    out!("  reconcile: {remediation}");
                }
                out!("  policy effect: advisory only; no gate or submit state changed");
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
                out!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "schema_version": crate::EXTERNAL_EVENT_SCHEMA_VERSION,
                        "events": events,
                        "includes_terminal": parsed.all,
                        "limit": 500,
                    }))?
                );
            } else if events.is_empty() {
                out!("No matching external coordination events.");
            } else {
                out!("{:<5} {:<24} {:<22} OWNER", "ID", "TYPE", "STATUS");
                for event in events {
                    out!(
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
                out!("{}", serde_json::to_string_pretty(&event)?);
            } else {
                out!("External event {}:", event.id);
                out!(
                    "  type/status: {} / {}",
                    event.event_type,
                    event.status.as_str()
                );
                out!("  repository: {}", event.repository);
                out!("  PR/commit: #{} / {}", event.pr_number, event.commit_sha);
                out!(
                    "  owner: {}",
                    event
                        .session_id
                        .map(|session| format!("session {session}"))
                        .unwrap_or_else(|| "unresolved".into())
                );
                out!("  policy effect: advisory only");
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
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                out!(
                    "External event {} reconciled as {} (reason stored as SHA-256 only).",
                    report.event.id,
                    report.event.status.as_str()
                );
                out!("Policy effect: advisory only; no gate or submit state changed.");
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
                    out!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    out!(
                        "Captured {} report: {}",
                        kind.as_str(),
                        result.path.as_deref().unwrap_or("-")
                    );
                    out!("SHA-256: {}", result.sha256);
                    out!("Review this local report before any later filing step.");
                }
            }
            Ok(())
        }
        Some("list") if parsed.positional.len() == 1 => {
            let main_root = report_main_root()?;
            let report = crate::list_reports(&main_root)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else if report.reports.is_empty() && report.invalid.is_empty() {
                out!("No captured reports.");
            } else {
                if report.reports.is_empty() {
                    out!("No valid captured reports.");
                } else {
                    out!(
                        "CAPTURED_AT    KIND          STATE     VERSION          DIGEST       PATH"
                    );
                    for item in &report.reports {
                        out!(
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
                out!("{}", serde_json::to_string_pretty(&inspection)?);
            } else {
                out!("Report: {}", inspection.summary.path);
                out!("  title: {}", inspection.summary.title);
                out!("  captured at: {}", inspection.summary.captured_at);
                out!("  kind: {}", inspection.summary.kind.as_str());
                out!("  version: {}", inspection.summary.version);
                out!("  digest: {}", inspection.summary.digest);
                out!(
                    "  filing state: {}",
                    match inspection.summary.filing_state {
                        crate::ReportFilingState::Unfiled => "unfiled",
                        crate::ReportFilingState::Filed => "filed",
                    }
                );
                out!("\n{}", serde_json::to_string_pretty(&inspection.report)?);
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
                    out!("{}", serde_json::to_string_pretty(&written)?);
                } else {
                    out!("Rendered reviewed report: {}", written.path);
                    out!("SHA-256: {}", written.sha256);
                    if !written.valid {
                        out!(
                            "Edit the required unfilled sections before filing: {}",
                            written.missing_required.join(", ")
                        );
                    }
                }
            } else if parsed.json {
                out!("{}", serde_json::to_string_pretty(&rendered)?);
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
                out!("{}", serde_json::to_string_pretty(&filed)?);
            } else {
                match filed.state {
                    crate::ReportFileState::Filed => {
                        out!(
                            "Filed {} as {}#{}",
                            filed.path,
                            filed.repository,
                            filed.issue_number.unwrap_or_default()
                        );
                        if let Some(url) = filed.issue_url.as_deref() {
                            out!("Issue: {url}");
                        }
                        out!("Operation: {}", filed.operation_id);
                    }
                    crate::ReportFileState::ReconciliationRequired => {
                        out!(
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
    out!(
        "{} generation {} — {} until {}",
        lease.lease_id,
        lease.generation,
        lease.state.as_str(),
        lease.expires_at
    );
    for allocation in &lease.allocations {
        out!(
            "  {:<20} {:<14} {}",
            allocation.key,
            allocation.kind,
            allocation.value
        );
    }
}

fn render_submission_plan(plan: &crate::SubmissionPlan, checkout: &crate::GitRepo) {
    out!(
        "Submitting session {} — HEAD {} onto integration {}",
        plan.session_id,
        short_sha(&plan.session_head),
        short_sha(&plan.integration_head)
    );
    out!(
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

    out!(
        "  merged-tree delta: {} file(s)",
        plan.merged_tree_paths.len()
    );
    for path in plan.merged_tree_paths.iter().take(10) {
        out!("    {path}");
    }
    if plan.merged_tree_paths.len() > 10 {
        out!("    ... and {} more", plan.merged_tree_paths.len() - 10);
    }
    for warning in &plan.warnings {
        out!("  warning: {warning}");
    }
}

fn render_submission_group<'a>(
    label: &str,
    commits: impl Iterator<Item = &'a crate::SubmissionCommitProvenance>,
    checkout: &crate::GitRepo,
) {
    let commits = commits.collect::<Vec<_>>();
    out!("  {label}: {}", commits.len());
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
        out!("    {} {subject} [{state}]", short_sha(&commit.commit));
    }
    if commits.len() > 10 {
        out!("    ... and {} more", commits.len() - 10);
    }
}

fn short_sha(value: &str) -> &str {
    &value[..12.min(value.len())]
}

/// `console` resolves the repository from the current directory rather than
/// from a session id: an operator starting a dev server is not necessarily in
/// a broker session, and requiring one would put coordination behind exactly
/// the step people skip.
fn console_context() -> Result<(crate::ConsoleConfig, String, PathBuf, PathBuf), UsageError> {
    let cwd = std::env::current_dir().map_err(|error| UsageError::Message(error.to_string()))?;
    let repo = crate::GitRepo::discover(&cwd).map_err(|error| {
        UsageError::Message(format!("console requires a git checkout: {error}"))
    })?;
    let worktree_root = repo.root().to_path_buf();
    let main_root = repo
        .main_root()
        .map_err(|error| UsageError::Message(error.to_string()))?;
    // The same `origin` anchor gates use, so one repository keeps one key
    // across gate leases and console leases alike. Anchored on the primary
    // checkout rather than this one: with no `origin` the fingerprint falls
    // back to a directory name, and taking it from a linked worktree would
    // give every worktree its own key -- which is exactly the contention
    // `singular` exists to create.
    let anchor = crate::GitRepo::discover(&main_root).unwrap_or(repo);
    let repository = crate::gates::git_origin_fingerprint(&anchor);
    let config = crate::ConsoleConfig::load(&main_root);
    Ok((config, repository, main_root, worktree_root))
}

fn run_console(parsed: Parsed) -> Result<(), UsageError> {
    let action = parsed
        .positional
        .first()
        .map(String::as_str)
        .unwrap_or("status");
    let (config, repository, main_root, worktree_root) = console_context()?;
    let identity = crate::console_identity(&config, &repository, &main_root, &worktree_root);
    match action {
        "status" => {
            let coordinator = crate::HostResourceCoordinator::open_read_only_default()?;
            let leases = crate::console_leases(&coordinator.list(false)?, &repository);
            let canonical_fingerprint = crate::worktree_fingerprint(&main_root);
            if parsed.json {
                let running: Vec<_> = leases
                    .iter()
                    .map(|lease| {
                        serde_json::json!({
                            "lease_id": lease.lease_id,
                            "port": crate::console_port(lease),
                            "worktree_fingerprint": lease.worktree_fingerprint,
                            "canonical": lease.worktree_fingerprint == canonical_fingerprint,
                            "state": lease.state.as_str(),
                            "holder_pid": lease.holder_pid,
                            "expires_at": lease.expires_at,
                        })
                    })
                    .collect();
                out!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "identity": identity,
                        "canonical_checkout": main_root,
                        "this_checkout": worktree_root,
                        "running": running,
                    }))?
                );
            } else {
                out!("Console mode: {}", config.mode.as_str());
                out!("Canonical checkout: {}", main_root.display());
                out!(
                    "This checkout: {} ({})",
                    worktree_root.display(),
                    if identity.canonical {
                        "canonical"
                    } else {
                        "agent worktree — not canonical"
                    }
                );
                if leases.is_empty() {
                    out!("Running consoles: none");
                } else {
                    out!("Running consoles: {}", leases.len());
                    for lease in &leases {
                        out!(
                            "  {:<10} port {:<6} pid {:<8} {}{}",
                            lease.state.as_str(),
                            crate::console_port(lease).unwrap_or("-"),
                            lease
                                .holder_pid
                                .map_or_else(|| "-".into(), |pid| pid.to_string()),
                            &lease.worktree_fingerprint[..12.min(lease.worktree_fingerprint.len())],
                            if lease.worktree_fingerprint == canonical_fingerprint {
                                " (canonical)"
                            } else {
                                " (worktree)"
                            }
                        );
                    }
                }
            }
        }
        "plan" => {
            let Some(request) =
                crate::console_request(&config, &repository, &worktree_root, "plan", None)
            else {
                return unmanaged_notice(parsed.json, "plan");
            };
            let coordinator = crate::HostResourceCoordinator::open_read_only_default()?;
            let plan = coordinator.plan(&request)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                out!(
                    "Console plan ({}) — {} (advisory; run is authoritative)",
                    config.mode.as_str(),
                    if plan.available {
                        "available"
                    } else {
                        "blocked"
                    }
                );
                for allocation in &plan.proposed {
                    out!(
                        "  proposed {:<12} {:<14} {}",
                        allocation.key,
                        allocation.kind,
                        allocation.value
                    );
                }
                for conflict in &plan.conflicts {
                    out!(
                        "  conflict {:<12} {}",
                        conflict.resource_key,
                        conflict.reason
                    );
                }
            }
        }
        "run" => {
            if parsed.exec_command.is_empty() {
                return Err(UsageError::Message(
                    "console run requires -- <command> [args...]".into(),
                ));
            }
            let run_id = format!("pid{}", std::process::id());
            let Some(request) = crate::console_request(
                &config,
                &repository,
                &worktree_root,
                &run_id,
                Some(std::process::id()),
            ) else {
                // Unmanaged reserves nothing, so there is nothing to supervise.
                // Running the command anyway keeps one spelling of "start the
                // console" working in every mode.
                return run_unmanaged_console(&parsed.exec_command, &worktree_root, parsed.json);
            };
            let wait = parsed
                .wait
                .as_deref()
                .map(parse_resource_duration)
                .transpose()?
                .unwrap_or_default();
            let mut coordinator = crate::HostResourceCoordinator::open_default()?;
            let json = parsed.json;
            let report = coordinator.run_supervised(
                &request,
                wait,
                &parsed.exec_command,
                parsed.cleanup_command.as_deref(),
                &worktree_root,
                |message| {
                    if json {
                        eprintln!(
                            "{}",
                            serde_json::json!({
                                "type": "console_run_event",
                                "request_id": request.request_id,
                                "message": message,
                            })
                        );
                    } else {
                        eprintln!("console: {message}");
                    }
                },
            );
            let report = match report {
                Ok(report) => report,
                // Contention here is the feature, not a fault: in `singular`
                // it means a console is already serving this repository. Say
                // which one instead of reporting a bare resource conflict.
                Err(crate::HostResourceRunError::Resource(
                    crate::HostResourceError::Conflict { conflicts, .. },
                )) => {
                    let coordinator = crate::HostResourceCoordinator::open_read_only_default()?;
                    let running = crate::console_leases(&coordinator.list(false)?, &repository);
                    let mut message =
                        String::from("a console is already running for this repository");
                    for lease in &running {
                        message.push_str(&format!(
                            "\n  port {} pid {} (aethyme broker console status)",
                            crate::console_port(lease).unwrap_or("-"),
                            lease
                                .holder_pid
                                .map_or_else(|| "-".into(), |pid| pid.to_string()),
                        ));
                    }
                    if running.is_empty() {
                        for conflict in &conflicts {
                            message.push_str(&format!(
                                "\n  {} {}",
                                conflict.resource_key, conflict.reason
                            ));
                        }
                    }
                    return Err(UsageError::Message(message));
                }
                Err(error) => return Err(error.into()),
            };
            if json {
                eprintln!("{}", serde_json::to_string(&report)?);
            } else {
                eprintln!(
                    "console: child={} final={}",
                    report.child_exit_code,
                    report.final_lease_state.as_str()
                );
            }
            let exit = if report.child_exit_code != 0 {
                report.child_exit_code
            } else if report.authority_lost
                || report.final_lease_state != crate::HostLeaseState::Released
            {
                70
            } else {
                0
            };
            if exit != 0 {
                return Err(UsageError::SilentExit(exit));
            }
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown console action {other:?}; expected status, plan, or run"
            )));
        }
    }
    Ok(())
}

fn unmanaged_notice(json: bool, action: &str) -> Result<(), UsageError> {
    if json {
        out!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "mode": "unmanaged",
                "action": action,
                "reserved": serde_json::Value::Null,
            }))?
        );
    } else {
        out!("Console mode: unmanaged — nothing is reserved and nothing is coordinated.");
    }
    Ok(())
}

/// Unmanaged still runs the command, so a repository can opt out of
/// coordination without every operator learning a second way to start.
fn run_unmanaged_console(command: &[String], cwd: &Path, json: bool) -> Result<(), UsageError> {
    if !json {
        eprintln!("console: unmanaged mode — no lease, no port reservation");
    }
    let status = std::process::Command::new(&command[0])
        .args(&command[1..])
        .current_dir(cwd)
        .status()
        .map_err(|error| UsageError::Message(error.to_string()))?;
    // A signal-killed child reports no code; 70 keeps it distinguishable from
    // a clean exit rather than collapsing to success.
    let code = status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .unwrap_or(70);
    if code != 0 {
        return Err(UsageError::SilentExit(code));
    }
    Ok(())
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
                    out!("{}", serde_json::to_string_pretty(&plan)?);
                } else {
                    out!(
                        "Request {} — {} (advisory; acquire is authoritative)",
                        plan.request_id,
                        if plan.available {
                            "available"
                        } else {
                            "blocked"
                        }
                    );
                    for allocation in &plan.proposed {
                        out!(
                            "  proposed {:<20} {:<14} {}",
                            allocation.key,
                            allocation.kind,
                            allocation.value
                        );
                    }
                    for conflict in &plan.conflicts {
                        out!(
                            "  conflict {:<20} {}",
                            conflict.resource_key,
                            conflict.reason
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
                        out!(
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
                    out!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "lease": grant.lease,
                            "grant_path": parsed.grant_out,
                        }))?
                    );
                } else if parsed.json {
                    out!("{}", serde_json::to_string_pretty(&grant)?);
                } else {
                    render_host_lease(&grant.lease);
                    if let Some(path) = parsed.grant_out {
                        out!("Private grant: {}", path.display());
                    } else {
                        out!("Ownership token: {}", grant.ownership_token);
                        out!("Store the complete JSON grant privately for renew/release.");
                    }
                }
            }
        }
        "renew" | "release" => {
            let mut coordinator = crate::HostResourceCoordinator::open_default()?;
            let argument = parsed.positional.get(1).ok_or_else(|| {
                UsageError::Message(format!("resources {action} requires <grant.json>"))
            })?;
            let path = PathBuf::from(argument);
            // A lease id here is a natural mistake, and reporting it as a missing
            // file sends the operator looking for the wrong thing (issue #139).
            if !path.exists() {
                if let Some(lease) = coordinator
                    .list(false)?
                    .into_iter()
                    .find(|lease| &lease.lease_id == argument)
                {
                    return Err(UsageError::Message(format!(
                        "resources {action} takes the grant JSON written at acquire, not a lease \
                         id; {argument} is a {} lease. The grant carries the ownership token that \
                         authorizes {action}, and a holder that died leaves none to reuse -- \
                         reclaim that lease instead: aethyme broker resources reconcile \
                         {argument} --confirm {}",
                        lease.state.as_str(),
                        lease.generation,
                    )));
                }
            }
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
                out!("{}", serde_json::to_string_pretty(&grant)?);
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
                out!("{}", serde_json::to_string_pretty(&leases)?);
            } else if leases.is_empty() {
                out!("No active or quarantined host resource leases.");
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
                out!("{}", serde_json::to_string_pretty(&lease)?);
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
    if parsed.required_mode.is_some() && subcommand != "readiness" {
        return Err(UsageError::Message(
            "--require is valid only with broker readiness".into(),
        ));
    }
    surface_command_advisories(subcommand, &parsed);

    match subcommand.as_str() {
        "readiness" => {
            if !parsed.positional.is_empty() {
                return Err(UsageError::Message(
                    "readiness does not accept positional arguments".into(),
                ));
            }
            let cwd = std::env::current_dir()
                .map_err(|error| UsageError::Message(format!("cannot resolve cwd: {error}")))?;
            let report = crate::inspect_repository_readiness(&cwd);
            render_readiness_report(&report, parsed.json)?;
            if let Some(required) = parsed.required_mode.as_deref() {
                let required = crate::RepositoryOperatingMode::parse_requirement(required)
                    .ok_or_else(|| {
                        UsageError::Message(
                            "--require must be conflict-only, agent-ready, or parallel-ready"
                                .into(),
                        )
                    })?;
                if !report.meets(required) {
                    return Err(UsageError::Exit {
                        message: format!(
                            "repository mode {} does not meet required {}",
                            report.operating_mode.as_str(),
                            required.as_str()
                        ),
                        code: 1,
                    });
                }
            }
        }
        "worktree-root" => {
            if !parsed.positional.is_empty() {
                return Err(UsageError::Message(
                    "worktree-root does not accept positional arguments".into(),
                ));
            }
            let broker = open_broker(parsed.read_only_snapshot)?;
            let plan = broker.worktree_root_plan()?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                out!("Repository: {}", plan.repository_root.display());
                out!("Repository key: {}", plan.repository_key);
                if let (Some(root), Some(source)) = (&plan.preferred_root, plan.preferred_source) {
                    out!(
                        "Preferred worktree root: {} ({})",
                        root.display(),
                        source.as_str()
                    );
                    out!(
                        "Scanner boundary: {}",
                        if plan.preferred_outside_repository {
                            "outside the repository"
                        } else {
                            "invalid: preferred root resolves inside the repository"
                        }
                    );
                } else {
                    out!("Preferred worktree root: unavailable");
                }
                out!(
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
            let agent_identity = session_agent_identity(parsed.agent.as_deref());
            let report = broker.adopt_with_options(
                &path,
                parsed.task.as_deref(),
                crate::AdoptOptions {
                    mode,
                    sync_integration: parsed.sync_integration,
                    planned_paths: parsed.planned_paths,
                },
                agent_identity.as_deref(),
            )?;
            for renamed in &report.renamed_targets {
                out!(
                    "Renamed target: {} is now {}{}",
                    renamed.from,
                    renamed.to,
                    match (renamed.promoted_entry_id, renamed.promoted_session_id) {
                        (Some(entry), Some(session)) =>
                            format!(" (queue entry {entry}, session {session})"),
                        _ => String::new(),
                    }
                );
                out!("  port this session's changes onto the new path before submitting");
            }
            let session = &report.session;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                match report.outcome {
                    crate::AdoptOutcome::Created => out!(
                        "Created session {} on the existing worktree — {} on branch {}",
                        session.id,
                        session.worktree_path,
                        session.branch
                    ),
                    crate::AdoptOutcome::Reused => out!(
                        "Reusing session {} — worktree {} on branch {}",
                        session.id,
                        session.worktree_path,
                        session.branch
                    ),
                    crate::AdoptOutcome::Replaced => out!(
                        "Replaced the prior session with session {} on the existing worktree — {} on branch {}",
                        session.id,
                        session.worktree_path,
                        session.branch
                    ),
                }
                if std::path::Path::new(&session.worktree_path) == broker.main_root() {
                    out!(
                        "note: main-checkout session — verification is advisory here \
                         (commits land on main before gates run); use a worktree \
                         session for enforced verification."
                    );
                }
                // Pre-existing uncommitted work is not this session's, but the
                // repository's own pre-push gate validates the whole snapshot,
                // so it will fail the first push for reasons the session cannot
                // see. Saying so at adopt time is cheaper than discovering it
                // one rejected push at a time.
                if let Ok(repo) =
                    crate::GitRepo::discover(std::path::Path::new(&session.worktree_path))
                    && let Ok(dirty) = repo.dirty_paths()
                    && !dirty.is_empty()
                {
                    let shown = dirty.iter().take(5).cloned().collect::<Vec<_>>();
                    out!(
                        "warning: {} uncommitted path(s) already present in this checkout \
                         before the session began: {}{}",
                        dirty.len(),
                        shown.join(", "),
                        if dirty.len() > shown.len() {
                            format!(", and {} more", dirty.len() - shown.len())
                        } else {
                            String::new()
                        }
                    );
                    out!(
                        "         they are not owned by this session, and a repository \
                         pre-push gate validates the whole snapshot — commit or set them \
                         aside, or adopt an isolated worktree instead."
                    );
                }
                if let Some(sync) = &report.integration_sync {
                    let summary = match sync.outcome {
                        crate::AdoptIntegrationSyncOutcome::AlreadyCurrent => "already current",
                        crate::AdoptIntegrationSyncOutcome::FastForwarded => "fast-forwarded",
                    };
                    out!(
                        "Integration synchronization: {summary} ({} -> {}, {} at {})",
                        short_commit(&sync.before_head),
                        short_commit(&sync.after_head),
                        sync.integration_branch,
                        short_commit(&sync.integration_head),
                    );
                }
                if let Some(drift) = &report.integration_drift {
                    out!(
                        "Integration drift: {} (session HEAD {}, {} HEAD {}; {} ahead, {} behind)",
                        drift.relation.as_str(),
                        short_commit(&drift.session_head),
                        drift.integration_branch,
                        short_commit(&drift.integration_head),
                        drift.ahead_commits,
                        drift.behind_commits,
                    );
                    if !drift.overlapping_changed_paths.is_empty() {
                        out!("Overlapping changed paths:");
                        for path in &drift.overlapping_changed_paths {
                            out!("  {path}");
                        }
                    }
                    if let Some(warning) = &drift.warning {
                        out!("Warning: {warning}");
                    }
                    out!("Safe next action: {}", drift.safe_next_action);
                }
                render_planned_explicit_leases(&report.planned_explicit_leases);
                render_preparation_status(&report.preparation, false)?;
            }
        }
        "start" => {
            let task = parsed
                .task
                .ok_or(UsageError::Message("start requires --task".into()))?;
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let agent_identity = session_agent_identity(parsed.agent.as_deref());
            let report = broker.start_worktree_with_planned_paths(
                &task,
                &parsed.planned_paths,
                agent_identity.as_deref(),
            )?;
            let session = &report.session;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                out!(
                    "Started session {} — worktree {} on branch {}",
                    session.id,
                    session.worktree_path,
                    session.branch
                );
                out!(
                    "Start base: {} at {} ({})",
                    report.start_base.ref_name,
                    short_commit(&report.start_base.commit),
                    report.start_base.evidence.as_str()
                );
                // Integration is normally ahead of the default branch. Behind
                // means it stopped following, and every session cut from it
                // inherits the gap — silently, because the line above looks
                // identical either way.
                if let Some(behind) = report.start_base.behind_default_commits
                    && behind > 0
                {
                    out!(
                        "warning: this base is {behind} commit(s) behind {}; a branch cut \
                         from it carries that gap into its pull request",
                        report
                            .start_base
                            .default_ref
                            .as_deref()
                            .unwrap_or("the default branch")
                    );
                    out!(
                        "         inspect with `aethyme broker integration status`, or start \
                         from the default branch if integration is not the base you want."
                    );
                }
                render_worktree_placement(&report.worktree_placement);
                render_planned_explicit_leases(&report.planned_explicit_leases);
                render_preparation_status(&report.preparation, false)?;
                out!("Worktree: cd {}", session.worktree_path);
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
            let agent_identity = session_agent_identity(parsed.agent.as_deref());
            let report = broker.start_agent_report(&task, &cmd, agent_identity.as_deref())?;
            let session = &report.session;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                out!(
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
        "reclaim" => run_reclaim(parsed)?,
        "deliveries" => run_deliveries(parsed)?,
        "review" => run_review(parsed)?,
        "prepare" => {
            let session_id = parsed
                .session
                .ok_or_else(|| UsageError::Message("prepare requires --session <id>".into()))?;
            match parsed.positional.first().map(String::as_str) {
                Some("status") => {
                    if parsed.positional.len() != 1 {
                        return Err(UsageError::Message(
                            "prepare status accepts no additional arguments".into(),
                        ));
                    }
                    if parsed.offline || parsed.wait.is_some() {
                        return Err(UsageError::Message(
                            "--offline and --wait apply only to preparation execution".into(),
                        ));
                    }
                    let broker = open_broker(true)?;
                    let status = broker.preparation_status(session_id)?;
                    render_preparation_status(&status, parsed.json)?;
                }
                None => {
                    let wait = parsed
                        .wait
                        .as_deref()
                        .map(parse_resource_duration)
                        .transpose()?
                        .unwrap_or_default();
                    let mut broker = open_broker(parsed.read_only_snapshot)?;
                    let report = broker.prepare_session(session_id, parsed.offline, wait)?;
                    if parsed.json {
                        out!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        out!(
                            "Preparation {:?} for session {} (digest {})",
                            report.state,
                            report.session_id,
                            short_sha(&report.digest)
                        );
                        for step in &report.steps {
                            out!(
                                "  {}: {} (exit {:?})",
                                step.name,
                                if step.succeeded { "passed" } else { "failed" },
                                step.exit_code
                            );
                        }
                        if report.shared_cache_coordinated {
                            out!("Shared cache: coordinated host-wide");
                        }
                        out!("Next: {}", report.next_action);
                    }
                }
                Some(other) => {
                    return Err(UsageError::Message(format!(
                        "unknown prepare action {other:?}; expected status or no action"
                    )));
                }
            }
        }
        "console" => run_console(parsed)?,
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
                out!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "agents": views,
                        "overlaps": overlaps,
                    }))?
                );
            } else if views.is_empty() {
                out!("No live sessions. Start one with `aethyme broker start --task \"...\"`.");
            } else {
                out!(
                    "{:<4} {:<8} {:<8} {:<24} TASK",
                    "ID",
                    "STATUS",
                    "ORIGIN",
                    "BRANCH"
                );
                for view in views {
                    out!(
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
                        out!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "leases": leases,
                                "overlaps": overlaps,
                            }))?
                        );
                    } else if leases.is_empty() {
                        out!("No active leases.");
                    } else {
                        out!("{:<4} {:<9} PATH", "SID", "KIND");
                        for lease in leases {
                            out!(
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
                        out!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        out!("Session {session} claimed {path}.");
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
                        out!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        out!(
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
                            out!(
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
                            out!(
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
                        out!("{{\"released\":{}}}", serde_json::to_string(path)?);
                    } else {
                        out!("Session {session} released {path}.");
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
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                out!(
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
                    out!("  touched paths: none");
                } else {
                    out!("  touched paths: {}", capped_join(&report.touched_paths, 8));
                }
                if !report.newly_dirty_paths.is_empty() {
                    out!(
                        "  newly dirty: {}",
                        capped_join(&report.newly_dirty_paths, 8)
                    );
                }
                if !report.modified_preexisting_dirty_paths.is_empty() {
                    out!(
                        "  changed while already dirty: {}",
                        capped_join(&report.modified_preexisting_dirty_paths, 8)
                    );
                }
                if !report.outside_lease_paths.is_empty() {
                    out!(
                        "  outside explicit leases: {}",
                        capped_join(&report.outside_lease_paths, 8)
                    );
                }
                if !report.foreign_paths.is_empty() {
                    out!(
                        "  adoption-time foreign paths: {}",
                        capped_join(&report.foreign_paths, 8)
                    );
                }
            }
            if !report.ok {
                // `ok` is `command_success && audit.ok`, so the two causes are
                // already separable. Reporting both as an ownership failure
                // sends the reader to debug leases when the wrapped command
                // simply exited non-zero for its own reasons.
                let guard_refused = !report.outside_lease_paths.is_empty()
                    || !report.foreign_paths.is_empty()
                    || !report.modified_preexisting_dirty_paths.is_empty();
                return Err(UsageError::Message(
                    match (report.command_success, guard_refused) {
                        (true, _) => "guarded exec refused: the command changed paths outside \
                                  this session's ownership (listed above)"
                            .to_string(),
                        (false, false) => format!(
                            "guarded exec: the command exited {} — the guard found no ownership \
                         violation, so this is the command's own failure",
                            report
                                .exit_code
                                .map(|code| code.to_string())
                                .unwrap_or_else(|| "by signal".into())
                        ),
                        (false, true) => format!(
                            "guarded exec: the command exited {}, and it changed paths outside \
                         this session's ownership (listed above)",
                            report
                                .exit_code
                                .map(|code| code.to_string())
                                .unwrap_or_else(|| "by signal".into())
                        ),
                    },
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
            let queue_wait = match (parsed.no_wait, parsed.queue_timeout_seconds) {
                (true, Some(_)) => {
                    return Err(UsageError::Message(
                        "--no-wait and --queue-timeout are mutually exclusive".into(),
                    ));
                }
                (true, None) => crate::QueueWait::Refuse,
                (false, Some(seconds)) => crate::QueueWait::Seconds(seconds),
                (false, None) => crate::QueueWait::Forever,
            };
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let report = broker.run_coordinated_operation_with_wait(request, queue_wait)?;
            render_coordinated_operation(&report, parsed.json)?;
            // After the coordinated operation returned, so the repository
            // write lock is released. Starting the watch inside it would hold
            // that lock across a provider call (#138). Opt-in only: a fleet
            // that watched every PR it opens would deliver interruptions to
            // agents that never asked for them.
            if let Some(number) = report.created_pull_request {
                let session = report.operation.session_id;
                let repository = report
                    .github_target
                    .as_ref()
                    .map(|target| target.display_slug.clone())
                    .unwrap_or_else(|| report.operation.repository.clone());
                let root = broker.main_root().to_path_buf();
                if crate::pr_monitoring_is_active(&root, session) {
                    // Comments and reviews, not checks: this exists to route
                    // human review back to the agent, and check churn on a
                    // busy PR would bury it. An empty list is rejected as
                    // meaning nothing, so the default must be explicit.
                    match broker.start_pull_request_watch(
                        session,
                        &repository,
                        number,
                        vec![
                            crate::PullRequestActivityKind::Comment,
                            crate::PullRequestActivityKind::Review,
                        ],
                        300,
                        &crate::GithubCliPullRequestWatchProvider,
                        now_ms(),
                    ) {
                        Ok(watch) => out!(
                            "Watching pull request {number} for session {session} (watch {})",
                            watch.id
                        ),
                        // Monitoring is an addition to the work, never a reason
                        // to report the pull request itself as failed.
                        Err(error) => {
                            out!("Pull request {number} opened; watch not started: {error}")
                        }
                    }
                } else {
                    out!(
                        "Pull request {number} opened. PR monitoring is off for session {session}; enable with:"
                    );
                    out!("  aethyme broker watch pr monitoring activate --session {session}");
                }
            }
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
                    if !parsed.read_only_snapshot {
                        broker.refresh_maintainer_recommendations()?;
                    }
                    let report = broker.advisory_list(parsed.all)?;
                    if !parsed.read_only_snapshot {
                        broker.record_advisories_shown(
                            &report.advisories,
                            crate::AdvisoryDeliverySurface::Inventory,
                        )?;
                    }
                    if parsed.json {
                        out!("{}", serde_json::to_string_pretty(&report)?);
                    } else if report.advisories.is_empty() {
                        out!("No outstanding advisories.");
                    } else {
                        out!(
                            "{:<5} {:<11} {:<9} {:<14} IDENTITY",
                            "ID",
                            "AUDIENCE",
                            "SEVERITY",
                            "STATE"
                        );
                        for advisory in &report.advisories {
                            out!(
                                "{:<5} {:<11} {:<9} {:<14} {}",
                                advisory.id,
                                advisory.audience.as_str(),
                                advisory.severity.as_str(),
                                advisory.resolution_state.as_str(),
                                advisory_text(&advisory.identity),
                            );
                        }
                        out!("Outstanding: {}", report.outstanding_count);
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
                        out!("{}", serde_json::to_string_pretty(&advisory)?);
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
                        out!("{}", serde_json::to_string_pretty(&advisory)?);
                    } else {
                        out!(
                            "Acknowledged advisory {}: {}",
                            advisory.id,
                            advisory_text(&advisory.identity)
                        );
                        out!("Projection refreshed: {}", crate::BROKER_ADVISORY_RELPATH);
                    }
                }
                Some("suppress") => {
                    if parsed.positional.len() != 2 {
                        return Err(UsageError::Message(ADVISORIES_SUPPRESS_USAGE.into()));
                    }
                    let id =
                        parse_advisory_id(parsed.positional.get(1), ADVISORIES_SUPPRESS_USAGE)?;
                    let advisory = broker.suppress_maintainer_advisory(id)?;
                    if parsed.json {
                        out!("{}", serde_json::to_string_pretty(&advisory)?);
                    } else {
                        out!(
                            "Suppressed maintainer advisory {}: {}",
                            advisory.id,
                            advisory_text(&advisory.identity)
                        );
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
                        out!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "schema_version": 1,
                                "summary": summary,
                                "metrics": metrics,
                            }))?
                        );
                    } else {
                        out!(
                            "Advisory delivery: {} shown, {} actioned, {} displays across {} surfaces.",
                            summary.shown_advisories,
                            summary.actioned_advisories,
                            summary.total_shows,
                            summary.surface_rows,
                        );
                        for metric in metrics {
                            out!(
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
                        "unknown advisories action {other:?} — expected list, show, ack, suppress, or metrics"
                    )));
                }
                None => {
                    return Err(UsageError::Message(
                        "advisories requires an action: list, show, ack, suppress, or metrics"
                            .into(),
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
                        out!("{}", serde_json::to_string_pretty(&plan)?);
                    } else {
                        out!(
                            "Remote: {} @ {}",
                            plan.remote_default_branch_ref,
                            short_commit(&plan.remote_default_branch_sha)
                        );
                        out!(
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
                        out!(
                            "Exposures: {} contained, {} remaining",
                            plan.contained_exposures.len(),
                            plan.remaining_exposures.len()
                        );
                        let eligible = plan
                            .advisories
                            .iter()
                            .filter(|advisory| advisory.eligible)
                            .count();
                        out!(
                            "Advisories: {} eligible, {} blocked by live leases",
                            eligible,
                            plan.advisories.len().saturating_sub(eligible)
                        );
                        for refusal in &plan.refusals {
                            out!("Refusal: {refusal}");
                        }
                        out!("Plan digest: {}", plan.digest);
                        if plan.safe {
                            out!(
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
                        out!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        out!(
                            "Verified {} at {} via operation {}.",
                            report.plan.remote_default_branch_ref,
                            report.plan.remote_default_branch_sha,
                            report.verification_operation.id
                        );
                        out!(
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
                        out!("{}", serde_json::to_string_pretty(&note)?);
                    } else {
                        out!(
                            "Sent broker note {} from session {} to session {}.",
                            note.id,
                            note.sender_session_id,
                            note.recipient_session_id
                        );
                    }
                }
                Some("list") if parsed.positional.len() == 1 => {
                    let recipient = parsed.session.ok_or_else(|| {
                        UsageError::Message("note list requires --session <recipient>".into())
                    })?;
                    let list = broker.session_note_list(recipient)?;
                    if parsed.json {
                        out!("{}", serde_json::to_string_pretty(&list)?);
                    } else if list.notes.is_empty() {
                        out!("No broker notes for session {recipient}.");
                    } else {
                        out!("{:<5} {:<8} {:<14} MESSAGE", "ID", "FROM", "STATE");
                        for note in &list.notes {
                            out!(
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
                        out!("Unread: {}", list.unread_count);
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
                        out!("{}", serde_json::to_string_pretty(&note)?);
                    } else {
                        out!("Acknowledged broker note {}.", note.id);
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
                        out!("{}", serde_json::to_string_pretty(&page)?);
                    } else if page.operations.is_empty() {
                        out!("No coordinated operations recorded.");
                    } else {
                        out!(
                            "{:<5} {:<8} {:<21} {:<22} SCOPE",
                            "ID",
                            "TOOL",
                            "STATUS",
                            "REPOSITORY"
                        );
                        for operation in page.operations {
                            out!(
                                "{:<5} {:<8} {:<21} {:<22} {}",
                                operation.id,
                                operation.provider.as_str(),
                                operation.status.as_str(),
                                operation.repository,
                                operation.scope,
                            );
                        }
                        if let Some(before_id) = page.next_before_id {
                            out!("More operations: pass --before {before_id}.");
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
                        out!("{}", serde_json::to_string_pretty(&report)?);
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
                        out!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        out!(
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
                    "gates requires an action: draft, validate, doctor, manifest, scope, affected, semantic, run, or pre-push"
                        .into(),
                ))?;
            if parsed.probe && action != "doctor" {
                return Err(UsageError::Message(
                    "--probe is valid only with broker gates doctor".into(),
                ));
            }
            match action {
                "draft" => {
                    let cwd = std::env::current_dir()
                        .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
                    let report = crate::init::draft_gates(&cwd)?;
                    if parsed.json {
                        out!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        for check in &report.checks {
                            out!(
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
                                    "timeout_seconds": g.timeout_seconds,
                                    "resources": g.resources,
                                    "resource_ttl_seconds": g.resource_ttl_seconds,
                                    "resource_wait_seconds": g.resource_wait_seconds,
                                    "managed_cache": g.managed_cache,
                                    "definition_hash": g.definition_hash,
                                })
                            })
                            .collect();
                        out!("{}", serde_json::to_string_pretty(&summary)?);
                    } else {
                        out!("gates.toml OK — {} gate(s), cheap-first:", gates.len());
                        for gate in gates {
                            out!(
                                "  [{}] {} — {} (triggers: {}{}; timeout: {}; resources: {}; definition: {})",
                                gate.cost,
                                gate.name,
                                gate.command,
                                if gate.triggers.is_empty() {
                                    "always".to_string()
                                } else {
                                    gate.triggers.join(", ")
                                },
                                if gate.cache { "" } else { "; cache: off" },
                                gate.timeout_seconds
                                    .map(|seconds| format!("{seconds}s"))
                                    .unwrap_or_else(|| "unbounded".into()),
                                gate.resources.len(),
                                &gate.definition_hash[..12],
                            );
                        }
                    }
                }
                "doctor" => {
                    if parsed.positional.len() != 1 {
                        return Err(UsageError::Message(
                            "gates doctor does not accept positional arguments".into(),
                        ));
                    }
                    if parsed.session.is_some() || parsed.all || parsed.no_cache {
                        return Err(UsageError::Message(
                            "gates doctor accepts --probe, --only <gate>, and --json; it does not use session, --all, or --no-cache"
                                .into(),
                        ));
                    }
                    if parsed.only.is_some() && !parsed.probe {
                        return Err(UsageError::Message(
                            "gates doctor --only <gate> requires --probe".into(),
                        ));
                    }
                    let cwd = std::env::current_dir()
                        .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
                    let checkout = crate::GitRepo::discover(&cwd)?;
                    let report = if parsed.probe {
                        crate::probe_gate_quality(
                            &checkout,
                            parsed.only.as_deref(),
                            &CliGateDoctorProgress,
                        )?
                    } else {
                        crate::inspect_gate_quality(&checkout)?
                    };
                    if parsed.json {
                        out!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        render_gate_doctor(&report);
                    }
                }
                "manifest" => {
                    let cwd = std::env::current_dir()
                        .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
                    let checkout = crate::GitRepo::discover(&cwd)?;
                    let head = parsed.head.as_deref().unwrap_or("HEAD");
                    let (head_sha, gates) = crate::load_gates_at_commit(&checkout, head)?;
                    let graph_policy =
                        crate::graph_integrity::load_graph_policy_at_commit(&checkout, &head_sha)?;
                    let manifest = crate::gate_scope_manifest_with_graph(&gates, &graph_policy);
                    if parsed.json {
                        out!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "policy_head_sha": head_sha,
                                "manifest": manifest,
                            }))?
                        );
                    } else {
                        out!(
                            "Gate scope manifest {} at {}",
                            &manifest.manifest_sha256[..12],
                            &head_sha[..12]
                        );
                        out!("  schema: {}", manifest.schema_version);
                        out!("  gates: {}", manifest.gates.len());
                        out!("  semantic suggestions enforced: false");
                        out!(
                            "  graph integrity: {} (policy {})",
                            if manifest.graph_integrity.enforced {
                                "enforced"
                            } else {
                                "disabled"
                            },
                            short_commit(&manifest.graph_integrity.policy_sha256)
                        );
                        for gate in manifest.gates {
                            out!(
                                "  [{}] {} (triggers: {}; cache: {}; timeout: {}; resources: {})",
                                gate.cost,
                                gate.name,
                                if gate.triggers.is_empty() {
                                    "always".into()
                                } else {
                                    gate.triggers.join(", ")
                                },
                                if gate.cache { "use" } else { "disabled" },
                                gate.timeout_seconds
                                    .map(|seconds| format!("{seconds}s"))
                                    .unwrap_or_else(|| "unbounded".into()),
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
                    let (head_sha, gates) = crate::load_gates_at_commit(&checkout, head)?;
                    let graph_policy =
                        crate::graph_integrity::load_graph_policy_at_commit(&checkout, &head_sha)?;
                    let report = crate::evaluate_gate_scope_with_graph(
                        &checkout,
                        &gates,
                        &graph_policy,
                        base,
                        head,
                    )?;
                    if parsed.json {
                        out!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        out!(
                            "Gate scope {}..{} (manifest {})",
                            &report.base_sha[..12],
                            &report.head_sha[..12],
                            &report.manifest_sha256[..12]
                        );
                        out!("  changed paths: {}", report.changed_paths.len());
                        out!(
                            "  graph integrity: {} (policy {})",
                            if report.graph_integrity.enforced {
                                "enforced"
                            } else {
                                "disabled"
                            },
                            short_commit(&report.graph_integrity.policy_sha256)
                        );
                        if report.selected_gates.is_empty() {
                            out!("  selected gates: none");
                        } else {
                            out!("  selected gates:");
                            for selection in report.selected_gates {
                                match selection.triggered_by {
                                    Some(path) => out!("    {} ({path})", selection.gate),
                                    None => out!("    {} (always)", selection.gate),
                                }
                            }
                        }
                        out!("  semantic suggestions: advisory, not included");
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
                        out!("{}", serde_json::to_string_pretty(&out)?);
                    } else if selections.is_empty() {
                        out!("No gates affected by this session's diff.");
                    } else {
                        for (gate, why) in selections {
                            match why {
                                Some(path) => out!("{gate}  (triggered by {path})"),
                                None => out!("{gate}  (always runs)"),
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
                        out!("{}", serde_json::to_string_pretty(&report)?);
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
                        out!("{}", serde_json::to_string_pretty(&outcomes)?);
                    } else {
                        for outcome in &outcomes {
                            out!(
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
                        out!("{}", serde_json::to_string_pretty(&outcomes)?);
                    } else if outcomes.is_empty() {
                        out!("No gates affected — nothing to run.");
                    } else {
                        let mut failed = false;
                        for outcome in &outcomes {
                            out!(
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
                        out!("{}", serde_json::to_string_pretty(&report)?);
                    } else if report.plan.pushed_sha.is_none() {
                        out!("Pre-push: deletion-only update; no content gates required.");
                    } else {
                        for outcome in &report.gate_outcomes {
                            out!(
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
                        out!(
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
                        out!(
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
                    out!(
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
                out!("{}", serde_json::to_string_pretty(&outcome)?);
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
                    out!(
                        "graph integrity: {:?} (tree {}, policy {}) — {}",
                        graph.status,
                        short_commit(&graph.tree_hash),
                        short_commit(&graph.policy_digest),
                        graph.reason
                    );
                    if !graph.changed_paths.is_empty() {
                        out!("  stale graph paths: {}", graph.changed_paths.join(", "));
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
                        out!(
                            "gate {:<20} {} (cached, tree {}, saved {})",
                            gate.gate,
                            gate_status_label(gate.status, gate.failure_class),
                            short_commit(&gate.tree_hash),
                            duration_label(gate.duration_ms)
                        );
                    } else {
                        out!(
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
                    crate::SubmissionGateVerificationStatus::NoConfiguration => out!(
                        "verification: conflict-only — no .aethyme/gates.toml exists in the submitted tree; 0 gates selected"
                    ),
                    crate::SubmissionGateVerificationStatus::NoGatesTriggered => out!(
                        "verification: no gate matched this diff ({} configured, 0 selected); review triggers with `aethyme broker gates affected --session {}`",
                        outcome.gate_verification.configured_gates,
                        outcome.entry.session_id
                    ),
                    crate::SubmissionGateVerificationStatus::Passed => out!(
                        "verification: {} selected gate(s) passed ({} executed, {} cached)",
                        outcome.gate_verification.selected_gates,
                        outcome.gate_verification.executed_gates,
                        outcome.gate_verification.cached_gates
                    ),
                    crate::SubmissionGateVerificationStatus::Failed => out!(
                        "verification: {} selected gate(s) did not all pass",
                        outcome.gate_verification.selected_gates
                    ),
                }
                if !outcome.no_changes {
                    out!("gate wall time: {}ms", gate_wall_ms);
                }
                if outcome.entry.status.as_str() == "verified"
                    && matches!(
                        outcome.gate_verification.status,
                        crate::SubmissionGateVerificationStatus::NoConfiguration
                            | crate::SubmissionGateVerificationStatus::NoGatesTriggered
                    )
                {
                    out!(
                        "entry {} → conflict-checked (eligible for manual promotion; no gate verification)",
                        outcome.entry.id
                    );
                } else {
                    out!(
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
                    // "Nothing pending" is the right summary only when nothing was
                    // set aside. Commits that predate the recorded baseline are not
                    // session-owned, and saying so here is what saves the reader
                    // from reading the plan JSON to find out why (issue #144).
                    let inherited = outcome
                        .submission_plan
                        .commits
                        .iter()
                        .filter(|commit| {
                            commit.ownership
                                == crate::SubmissionCommitOwnership::InheritedFromRecordedBaseline
                        })
                        .count();
                    if inherited > 0 {
                        out!(
                            "What now: no pending session-owned content remains to integrate, but \
                             {inherited} commit(s) on this branch predate the recorded baseline{} \
                             and are not session-owned, so they were not replayed. To submit them, \
                             re-adopt the worktree from a base that precedes them.",
                            outcome
                                .submission_plan
                                .recorded_baseline
                                .as_deref()
                                .map(|baseline| format!(
                                    " ({})",
                                    &baseline[..12.min(baseline.len())]
                                ))
                                .unwrap_or_default(),
                        );
                    } else {
                        out!(
                            "What now: no pending session-owned content remains to integrate; \
                             aethyme/integration was not moved and no gates ran."
                        );
                    }
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
                    out!(
                        "What now: aethyme/integration is at {integration} and contains this work. \
                         Your checkout and branches are untouched — keep working, or start \
                         a follow-up with `aethyme broker adopt --reuse --task \"...\"`, or \
                         finish safely with `aethyme broker finish --session {}`.",
                        outcome.entry.session_id,
                    );
                } else {
                    out!(
                        "What now: entry {} is verified but not promoted (manual mode). \
                         Promote with `aethyme broker promote --entry {}`.",
                        outcome.entry.id,
                        outcome.entry.id,
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
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                render_repair_report(&report);
            }
        }
        "representation" => run_representation(parsed)?,
        "main" => {
            let action = parsed.positional.first().map(String::as_str);
            let step = parsed.positional.get(1).map(String::as_str);
            if action != Some("reconcile") {
                return Err(UsageError::Message(
                    "main requires reconcile plan or reconcile apply".into(),
                ));
            }
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            match step {
                Some("plan") => {
                    if let Some(path) = parsed.write_resolution_template.as_deref() {
                        let template = broker.main_reconcile_resolution_template()?;
                        std::fs::write(path, serde_json::to_string_pretty(&template)?).map_err(
                            |source| {
                                UsageError::Message(format!(
                                    "cannot write {}: {source}",
                                    path.display()
                                ))
                            },
                        )?;
                        out!(
                            "Wrote {} resolution(s) needing a decision to {}",
                            template.resolutions.len(),
                            path.display()
                        );
                        return Ok(());
                    }
                    let document = load_main_reconcile_resolutions(&parsed)?;
                    let plan = broker.main_reconcile_plan_with(document.as_ref())?;
                    if parsed.json {
                        out!("{}", serde_json::to_string_pretty(&plan)?);
                    } else {
                        render_main_reconcile_plan(&plan, parsed.detail);
                    }
                }
                Some("apply") => {
                    let session = parsed.session.ok_or(UsageError::Message(
                        "main reconcile apply requires --session <id>".into(),
                    ))?;
                    let confirm = parsed.confirm.as_deref().ok_or(UsageError::Message(
                        "main reconcile apply requires --confirm <sha256>".into(),
                    ))?;
                    let document = load_main_reconcile_resolutions(&parsed)?;
                    let report =
                        broker.main_reconcile_apply_with(session, confirm, document.as_ref())?;
                    if parsed.json {
                        out!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        out!(
                            "Main reconciled: {} moved {} -> {}",
                            report.default_branch,
                            &report.moved_from[..12.min(report.moved_from.len())],
                            &report.moved_to[..12.min(report.moved_to.len())],
                        );
                        out!("  preserved pre-move tip: {}", report.preservation_ref);
                        out!(
                            "  {} represented commit(s) left behind, recoverable from that ref",
                            report.represented_commits
                        );
                    }
                }
                other => {
                    return Err(UsageError::Message(format!(
                        "unknown main reconcile step {other:?}; expected plan or apply"
                    )));
                }
            }
        }
        "promotion-record" => {
            let action =
                parsed
                    .positional
                    .first()
                    .map(String::as_str)
                    .ok_or(UsageError::Message(
                        "promotion-record requires plan or apply".into(),
                    ))?;
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            match action {
                "plan" => {
                    let plan = broker.promotion_record_plan()?;
                    if parsed.json {
                        out!("{}", serde_json::to_string_pretty(&plan)?);
                    } else {
                        render_promotion_record_plan(&plan);
                    }
                }
                "apply" => {
                    let confirm = parsed.confirm.as_deref().ok_or(UsageError::Message(
                        "promotion-record apply requires --confirm <sha256>".into(),
                    ))?;
                    let report = broker.promotion_record_apply(confirm)?;
                    if parsed.json {
                        out!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        out!(
                            "Promotion record recovery: {} restored",
                            report.restored.len()
                        );
                        for id in &report.restored {
                            out!("  entry {id} recorded as promoted");
                        }
                        for skip in &report.skipped {
                            out!("  skipped: {skip}");
                        }
                    }
                }
                other => {
                    return Err(UsageError::Message(format!(
                        "unknown promotion-record action {other:?}; expected plan or apply"
                    )));
                }
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
                        out!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        out!(
                            "Checkpoint recovery for session {}: {}",
                            session,
                            if report.safe { "safe" } else { "refused" }
                        );
                        out!(
                            "  old: {}",
                            report.old_checkpoint.as_deref().unwrap_or("missing")
                        );
                        out!(
                            "  proposed: {}",
                            report.proposed_checkpoint.as_deref().unwrap_or("missing")
                        );
                        out!(
                            "  session HEAD: {} ({}; {} ahead, {} behind)",
                            report.session_head,
                            report
                                .integration_relation
                                .map(|relation| relation.as_str())
                                .unwrap_or("unknown"),
                            report.ahead_commits,
                            report.behind_commits
                        );
                        out!("  pending commits: {}", report.pending_commits.len());
                        out!("  preservation branch: {}", report.preservation_branch);
                        for refusal in &report.refusals {
                            out!("  refusal: {refusal}");
                        }
                        if !report.next_actions.is_empty() {
                            out!("  recovery actions:");
                            for action in &report.next_actions {
                                out!("    {}: {}", action.kind, action.command);
                                out!("      {}", action.description);
                            }
                        }
                        out!("Plan digest: {}", report.digest);
                        if report.safe {
                            out!(
                                "Apply with: aethyme broker checkpoint apply --session {} --confirm {}",
                                session,
                                report.digest
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
                        out!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        out!(
                            "Re-anchored session {} at {} after preserving {}.",
                            session,
                            report.accepted_session_head,
                            report.preservation_ref
                        );
                        out!("Next: aethyme broker submit --session {session}");
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
                    out!("{}", serde_json::to_string_pretty(&page)?);
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
            let mut entries = broker.store().merge_queue()?;
            // Watching a submit in flight is the one polling loop agents run,
            // and the bare inventory grows without bound — it reached 13 KB
            // here, paid on every poll. `--active` answers "is it done yet" in
            // the few entries that can still change. The bare command keeps its
            // documented compatibility-inventory shape.
            if parsed.active {
                entries.retain(|entry| {
                    matches!(
                        entry.status,
                        crate::MergeStatus::Submitted
                            | crate::MergeStatus::Simulating
                            | crate::MergeStatus::Verified
                            | crate::MergeStatus::Conflict
                    )
                });
            }
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&entries)?);
            } else if entries.is_empty() {
                out!(
                    "{}",
                    if parsed.active {
                        "No queue entry is in flight."
                    } else {
                        "Merge queue is empty."
                    }
                );
            } else {
                out!("{:<4} {:<4} {:<11} HEAD", "ID", "SID", "STATUS");
                for entry in entries {
                    out!(
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
                out!("{{\"promoted\":{entry}}}");
            } else {
                out!("Promoted entry {entry} to the local integration branch.");
                out!("Next: aethyme broker ship plan --entry {entry}");
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
                    render_ship_plan(&report, parsed.json, parsed.detail)?;
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
                out!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                out!(
                    "Integration: {} @ {}",
                    status.integration_branch,
                    &status.integration_head[..12.min(status.integration_head.len())]
                );
                out!("Local main:  {}", short_commit(&status.main_head));
                if let (Some(upstream_ref), Some(upstream_head)) =
                    (&status.upstream_ref, &status.upstream_head)
                {
                    out!(
                        "Upstream:    {} @ {} ({})",
                        upstream_ref,
                        short_commit(upstream_head),
                        upstream_relation(
                            status.main_ahead_upstream_commits,
                            status.main_behind_upstream_commits,
                        )
                    );
                }
                out!("Summary: {}", status.summary.message);
                out!();
                render_status_advice(&status.advice);
                if !status.outstanding_advisories.is_empty() {
                    out!();
                    out!(
                        "Outstanding advisories: {}",
                        status.outstanding_advisories.len()
                    );
                    for advisory in status.outstanding_advisories.iter().take(10) {
                        out!(
                            "  {} [{}]: {}",
                            advisory.id,
                            advisory.severity.as_str(),
                            advisory_text(&advisory.identity),
                        );
                        out!(
                            "    inspect: aethyme broker advisories show {}",
                            advisory.id
                        );
                        out!(
                            "    acknowledge: aethyme broker advisories ack {}",
                            advisory.id
                        );
                    }
                    if status.outstanding_advisories.len() > 10 {
                        out!(
                            "  and {} more; inspect: aethyme broker advisories list",
                            status.outstanding_advisories.len() - 10
                        );
                    }
                }
                if !status.outstanding_entry_exposures.is_empty() {
                    out!();
                    out!(
                        "Publication exposures: {} promoted {} not yet verified on remote main",
                        status.outstanding_entry_exposures.len(),
                        plural(status.outstanding_entry_exposures.len(), "entry", "entries")
                    );
                    for exposure in status.outstanding_entry_exposures.iter().take(10) {
                        out!(
                            "  qid {} @ {}: {} {}",
                            exposure.queue_entry_id,
                            short_commit(&exposure.promotion_sha),
                            exposure.paths.len(),
                            plural(exposure.paths.len(), "path", "paths")
                        );
                    }
                    if status.outstanding_entry_exposures.len() > 10 {
                        out!(
                            "  and {} more",
                            status.outstanding_entry_exposures.len() - 10
                        );
                    }
                    out!("  inspect: aethyme broker exposures plan");
                }
                // A caller parked behind a wedged operation is inside a command
                // that never returns, so it cannot report its own wait. Status
                // is the out-of-band surface that can (issue #147).
                if !status.coordinated_operations.is_empty() {
                    out!();
                    let holders = status
                        .coordinated_operations
                        .iter()
                        .filter(|operation| operation.holding_lock)
                        .count();
                    out!(
                        "Coordinated operations: {} unresolved, {} holding a write lock",
                        status.coordinated_operations.len(),
                        holders
                    );
                    for operation in status.coordinated_operations.iter().take(10) {
                        let role = if operation.holding_lock {
                            "holding".to_string()
                        } else {
                            match operation.blocked_by {
                                Some(blocker) => format!("blocked by {blocker}"),
                                None => "queued".to_string(),
                            }
                        };
                        out!(
                            "  op {:<6} sess {:<4} {:<7} {:<28} {:<9} {:>8}  {}",
                            operation.id,
                            operation.session_id,
                            operation.provider,
                            operation.repository,
                            operation.status,
                            crate::operations::humanize_duration(operation.elapsed_seconds),
                            role
                        );
                    }
                    if status.coordinated_operations.len() > 10 {
                        out!("  and {} more", status.coordinated_operations.len() - 10);
                    }
                    out!("  inspect: aethyme broker operations list");
                }
                out!();
                if status.agents.is_empty() {
                    out!("No live sessions.");
                } else {
                    out!(
                        "{:<4} {:<8} {:<8} {:<24} TASK",
                        "ID",
                        "STATUS",
                        "ORIGIN",
                        "BRANCH"
                    );
                    for view in &status.agents {
                        out!(
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
                    out!();
                    out!("Planned explicit leases:");
                    for lease in explicit_leases {
                        out!("  session {}: {}", lease.session_id, lease.path);
                    }
                }
                let current_queue = status
                    .queue
                    .iter()
                    .filter(|entry| queue_status_is_current(entry.status))
                    .collect::<Vec<_>>();
                if !current_queue.is_empty() {
                    out!();
                    out!("Current merge queue:");
                    out!("{:<4} {:<4} {:<11} HEAD", "QID", "SID", "QSTATUS");
                    for entry in current_queue {
                        out!(
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
                    out!();
                    out!(
                        "Queue history: {total} terminal {} ({summary}).",
                        plural(total, "entry", "entries")
                    );
                    out!("  inspect: {}", status.queue_history.command);
                }
                if status.advisory_delivery.shown_advisories > 0 {
                    out!();
                    out!(
                        "Advisory delivery: {} shown, {} actioned, {} displays.",
                        status.advisory_delivery.shown_advisories,
                        status.advisory_delivery.actioned_advisories,
                        status.advisory_delivery.total_shows,
                    );
                    out!("  inspect: aethyme broker advisories metrics");
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
                    out!("{{\"pruned\":{removed}}}");
                } else {
                    out!("Pruned {removed} event(s) older than {keep_days} day(s).");
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
                        out!("{}", serde_json::to_string(event)?);
                    } else {
                        out!(
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
            // (calls, total_ms, total_output_bytes, calls_that_recorded_bytes).
            // The last field matters because lines written before output
            // accounting existed carry no size; averaging over every call would
            // silently understate the cost of the ones that do.
            let mut commands: std::collections::BTreeMap<String, (i64, i64, i64, i64)> =
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
                        let bytes = v.get("output_bytes").and_then(|b| b.as_i64());
                        let entry = commands.entry(name).or_insert((0, 0, 0, 0));
                        entry.0 += 1;
                        entry.1 += ms;
                        if let Some(bytes) = bytes {
                            entry.2 += bytes;
                            entry.3 += 1;
                        }
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
                    "commands": commands.iter().map(|(name, (count, ms, bytes, sized))| serde_json::json!({
                        "command": name, "count": count, "total_ms": ms,
                        "total_output_bytes": bytes, "output_sampled_calls": sized,
                    })).collect::<Vec<_>>(),
                });
                out!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                out!("Gate executions:");
                for (gate, runs, ms) in &executed {
                    out!("  {gate:<20} {runs} run(s), {ms}ms total");
                }
                out!(
                    "Cache hits: {} (≈{}s of checks skipped)",
                    cached.len(),
                    saved_ms / 1000
                );
                out!("Conflicts caught before any gate ran: {conflicts}");
                out!("Overlap warnings: {overlaps}");
                out!("Broker command overhead:");
                for (name, (count, ms, bytes, sized)) in &commands {
                    let output = if *sized == 0 {
                        "output not sampled".to_string()
                    } else {
                        format!("{} per call", human_bytes((*bytes / sized.max(&1)) as u64))
                    };
                    out!(
                        "  {name:<20} {count} call(s), {ms}ms total, {}ms avg, {output}",
                        ms / count.max(&1),
                    );
                }
                // Output size is what an agent pays per turn, so name the
                // worst offender rather than leaving it to be spotted in a table.
                if let Some((name, (_, _, bytes, sized))) = commands
                    .iter()
                    .filter(|(_, (_, _, _, sized))| *sized > 0)
                    .max_by_key(|(_, (_, _, bytes, sized))| bytes / sized.max(&1))
                {
                    out!(
                        "Largest agent-facing output: {name} at {} per call over {sized} sampled call(s)",
                        human_bytes((*bytes / sized.max(&1)) as u64)
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
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                out!("integrity: {}", report.integrity);
                out!(
                    "version: {} — {}",
                    report.version.status.as_str(),
                    report.version.message
                );
                if let Some(describe) = &report.version.binary.describe {
                    out!(
                        "  binary: aethyme {} ({describe})",
                        report.version.binary.version
                    );
                } else {
                    out!("  binary: aethyme {}", report.version.binary.version);
                }
                if let Some(path) = &report.version.binary.path {
                    out!("  path: {path}");
                }
                if report.version.repo_is_aethyme_source {
                    let integration = report
                        .version
                        .integration_describe
                        .as_deref()
                        .or(report.version.integration_head.as_deref())
                        .unwrap_or("unknown");
                    out!(
                        "  integration: {} {integration}",
                        report.version.integration_branch
                    );
                }
                if let Some(movement) = &report.integration_movement {
                    out!("integration movement: {}", movement.message);
                    out!(
                        "  head: {} @ {}",
                        movement.branch,
                        short_commit(&movement.head)
                    );
                    for session in movement.live_sessions.iter().take(5) {
                        out!(
                            "  live session {} {} {}",
                            session.id,
                            session.status.as_str(),
                            session.branch
                        );
                    }
                    if movement.live_sessions.len() > 5 {
                        out!(
                            "  and {} more live {}",
                            movement.live_sessions.len() - 5,
                            plural(movement.live_sessions.len() - 5, "session", "sessions")
                        );
                    }
                    for command in &movement.commands {
                        out!("  run: {command}");
                    }
                }
                if let Some(repair) = &report.version_repair {
                    out!(
                        "version repair: {} — {}",
                        repair.status.as_str(),
                        repair.message
                    );
                    if repair.attempted {
                        out!("  duration: {}ms", repair.duration_ms);
                        if let Some(code) = repair.exit_code {
                            out!("  exit: {code}");
                        }
                        for step in &repair.steps {
                            out!(
                                "  {} {}: {}",
                                step.component,
                                step.action,
                                if step.success { "pass" } else { "fail" }
                            );
                            out!("    command: {}", step.command.join(" "));
                            if let Some(code) = step.exit_code {
                                out!("    exit: {code}");
                            }
                        }
                        if repair.steps.is_empty() {
                            out!("  command: {}", repair.command.join(" "));
                        }
                    }
                    if !repair.stdout_tail.is_empty() {
                        out!("  stdout tail:");
                        for line in &repair.stdout_tail {
                            out!("    {line}");
                        }
                    }
                    if !repair.stderr_tail.is_empty() {
                        out!("  stderr tail:");
                        for line in &repair.stderr_tail {
                            out!("    {line}");
                        }
                    }
                }
                if report.missing_worktrees.is_empty() {
                    out!("worktrees: all live session worktrees exist");
                } else {
                    for id in &report.missing_worktrees {
                        out!("worktrees: session {id} worktree is missing (adopt gone stale?)");
                    }
                }
                if report.orphaned_pidfiles.is_empty() {
                    out!("gate runs: no orphaned pidfiles");
                } else {
                    for name in &report.orphaned_pidfiles {
                        out!("gate runs: orphaned pidfile removed: {name}");
                    }
                }
                out!(
                    "retention: {} rows, {} files, {} worktrees, {} retained, {} reclaimable; {} protected findings",
                    report.retention.candidate_rows,
                    report.retention.candidate_files,
                    report.retention.candidate_worktrees,
                    human_bytes(report.retention.estimated_retained_bytes),
                    human_bytes(report.retention.estimated_reclaimable_bytes),
                    report.retention.blockers,
                );
                if report.retention.over_retained_bytes_budget {
                    out!(
                        "  warning: retained storage exceeds the configured {} budget; review `aethyme broker gc plan`",
                        human_bytes(report.retention.policy.retained_bytes_budget)
                    );
                }
                if let Some(digest) = &report.retention.pending_recovery_digest {
                    out!("  recovery pending: aethyme broker gc apply --confirm {digest}");
                }
                if report.healthy() {
                    out!("doctor: healthy");
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
                out!("{}", serde_json::to_string_pretty(&report)?);
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
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                out!("Phase 1/3 — certify (read-only):");
                print_checks(&report.certify.checks);
                let Some(scaffold) = &report.scaffold else {
                    out!();
                    return Err(UsageError::Message(
                        "certification failed — fix the FAIL items above, then re-run \
                         `aethyme init` (nothing was written)"
                            .into(),
                    ));
                };
                out!();
                out!("Phase 2/3 — scaffold (deterministic, only-if-missing):");
                print_checks(&scaffold.checks);
                out!();
                out!("Phase 3/3 — gates draft (adaptive):");
                match &report.gates {
                    Some(gates) => print_checks(&gates.checks),
                    None => out!(
                        "{:<8} {:<28} .aethyme/gates.toml already present — drafting skipped",
                        "skip",
                        "gates.draft"
                    ),
                }
                out!();
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
                    out!("Already existed (untouched): {}", existing.join(", "));
                }
                if report.changed {
                    out!("Created this run:");
                    for check in write_checks
                        .iter()
                        .filter(|c| c.status == crate::init::CheckStatus::Created)
                    {
                        out!("  - {} — {}", check.id, check.detail);
                    }
                } else {
                    out!(
                        "Nothing created — this repository was already set up \
                         (init is idempotent)."
                    );
                }
                out!();
                out!("Repository initialized.");
                out!();
                out!(
                    "{}",
                    crate::render_readiness_text(&report.readiness).trim_end()
                );
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
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_checks(&report.checks);
                out!();
                if report.certified() {
                    if subcommand == "certify" {
                        out!("Certified (read-only — nothing written).");
                    } else {
                        out!("Scaffolding done — review the drafts, then run `aethyme certify`.");
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
                out!("{}", serde_json::to_string_pretty(&report)?);
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
                out!("{}", serde_json::to_string_pretty(&report)?);
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
                out!("{}", serde_json::json!({ "closed": session }));
            } else {
                out!(
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
                        out!("{}", serde_json::to_string_pretty(&plan)?);
                    } else {
                        render_gc_plan(&plan, parsed.detail);
                    }
                }
                "apply" => {
                    let confirm = parsed.confirm.as_deref().ok_or_else(|| {
                        UsageError::Message("gc apply requires --confirm <sha256>".into())
                    })?;
                    let report = broker.gc_apply(confirm)?;
                    if parsed.json {
                        out!("{}", serde_json::to_string_pretty(&report)?);
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
                    out!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    render_cleanup_sweep_report(&report, parsed.detail);
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
                    out!("{{\"cleaned\":{id}}}");
                } else {
                    out!("Cleaned session {id}.");
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
    if matches!(subcommand, "hooks" | "readiness") {
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
