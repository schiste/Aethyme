# CLI Reference

Last Updated: 2026-08-24

## Install

```bash
brew install schiste/tap/aethyme
aethyme --version
aethyme-engine-cli --version
```

Homebrew is the primary stable install and update path. It installs both
executables from one checksummed archive; update with `brew update` and
`brew upgrade aethyme`.

Without Homebrew:

```bash
curl -fsSL https://github.com/schiste/Aethyme/releases/latest/download/install.sh | sh
aethyme --version
aethyme-engine-cli --version
```

The installer establishes a versioned, paired layout for explicit native
updates. Aethyme has no self-update daemon or silent background upgrade path.
Use `sh -s -- --version
0.2.2` to pin a release, or follow the
[v0.2.2 upgrade guide](../guides/upgrading-to-v0.2.2.md) for source installs,
manifest signature verification, migration, and rollback.

`aethyme` and its required `aethyme-engine-cli` sibling are native Rust
binaries; no interpreter, virtualenv, or pip step is involved.
**`python -m src.cli` no longer exists** — the Python
package was deleted on 2026-08-01 (python-retirement Phase 6) with no
shim, and the old spelling fails with `No module named src`. Every
command below is native, and an unknown subcommand is an error (exit 2)
rather than a fallthrough.

## Global Options

- `--tenant-id`
- `--json`
- `--verbose`

## Product-Surface Tiers

The public product map is
[`../../../../docs/product-surface.md`](../../../../docs/product-surface.md).
This reference includes stable front-door commands, advanced public tools, and
internal or historical commands; do not treat every listed command as equal
product surface.

### Stable Front Door

- `aethyme deploy`
- `aethyme deploy verify`
- `aethyme deploy bridge`
- `aethyme deploy --local-only`
- `aethyme init`
- `aethyme certify`
- `aethyme broker status`
- `aethyme broker start`
- `aethyme broker adopt`
- `aethyme broker exec`
- `aethyme broker git`
- `aethyme broker gh`
- `aethyme broker operations`
- `aethyme broker advisories`
- `aethyme broker queue history`
- `aethyme broker submit`
- `aethyme broker repair`
- `aethyme broker finish`
- `aethyme broker handoff`
- `aethyme broker report capture`
- `aethyme broker report list`
- `aethyme broker report show`
- `aethyme broker report render`
- `aethyme broker report file`
- `aethyme broker ship plan`
- `aethyme broker ship execute`
- `aethyme broker integration status`
- `aethyme broker integration reconcile`
- `aethyme broker quick-test`
- `aethyme broker verify-loop`
- `aethyme update check`
- `aethyme update plan`
- `aethyme update execute`
- `aethyme deploy plan`
- `aethyme deploy execute`
- `aethyme upgrade plan`
- `aethyme upgrade apply`
- `aethyme explore`

### Advanced Public Tools

- `aethyme broker gates ...`
- `aethyme broker events`
- `aethyme broker metrics`
- `aethyme broker doctor`
- `aethyme broker leases ...`
- `aethyme broker pr check`
- `aethyme broker cleanup`
- `aethyme graph ...`
- `aethyme facts ...`
- `aethyme task ...`
- `aethyme analyze dead-code`
- `aethyme enhance deploy`
- `aethyme enhance verify`
- `aethyme repo experience-*`

### Internal Or Historical

Local eval harnesses, benchmark/report generators, storage implementation
details, SaaS-era material, and compatibility-only commands may be documented
for maintainers but should not lead user onboarding.

## Update Commands

```text
aethyme update check [--json]
aethyme update plan [--channel stable|preview] [--json]
aethyme update execute --confirm <manifest-sha256> [--json]
```

`check` and `plan` perform network access only when explicitly invoked. The
stable channel reads GitHub's latest non-prerelease manifest; the preview
channel discovers GitHub's latest published prerelease. Discovery is then
normalized to that release's version-specific manifest URL, so the saved plan
never executes against a moving alias. A plan records the current and target
versions, source SHA, installation provenance, full manifest digest, exact
platform archive URL/digest/size, engine protocol, and broker-storage
compatibility range, plus the repository deployment schema embedded in that
release.

Update authority follows installation provenance:

- Homebrew: report `brew upgrade aethyme`; never modify the Cellar.
- Aethyme installer: persist the reviewed plan and permit confirmed execution.
- Cargo: report the paired contributor reinstall commands.
- Manual archive or unknown: recommend adopting the managed installer.

`execute` requires the full reviewed manifest SHA-256. It re-downloads and
revalidates that exact manifest, verifies archive size and SHA-256, requires
exactly the two expected archive members, validates both embedded versions,
runs `aethyme broker quick-test` against the staged pair, then atomically swaps
one shared `current` symlink. The prior bundle remains available as the single
rollback bundle. A failed download, checksum, staged smoke, or activation
verification leaves or restores the earlier `current` link.

When run from a repository containing `.aethyme/broker.db`, execution opens
that database read-only and refuses a target whose advertised readable schema
range excludes the local schema. The updater does not scan the filesystem for
other repositories; back up every broker database that must remain rollback
safe before a compatibility-changing release.

Neither `--help`, normal broker commands, nor any background process performs
an update check.

## Repository Upgrade Commands

```text
aethyme upgrade plan [--repo <path>] [--local-only] [--resolution-file <path>] [--diff|--json]
aethyme upgrade apply [--repo <path>] [--local-only] [--resolution-file <path>] --confirm <plan-sha256> [--json]
aethyme upgrade recover [--repo <path>] --plan <plan-sha256> [--json]
```

Binary updates and repository migrations are intentionally separate. Run
`plan` in each enrolled repository after updating the binary pair. It is
read-only and binds the source HEAD, existing managed-state digest, proposed
content hashes and modes, resolution choices, compatibility decision, active
session contracts, dirty-path overlap classification, relevant leases, planned
paths, and embedded migrations into a full SHA-256. `--diff` renders the exact binary-capable Git patch locally;
`--json` emits the content-free structured plan and the patch SHA-256. The two
formats are intentionally exclusive. `apply` requires that exact plan digest,
a supported marker, no dirty path overlapping a proposed write, and no live
session during shared policy or gate migration. Disjoint dirty paths may remain
because they are excluded from proposal inputs and `apply` writes only the
exact reviewed outputs. Diff bodies and dirty file contents never enter broker
reports, events, metrics, or command telemetry.

`apply` runs under an exclusive upgrade lock, durably journals recoverable
before-bytes, installs sibling temporary files with atomic renames, verifies
every reviewed hash, and writes the repository marker last. If the process is
interrupted, `recover --plan <plan-sha256>` rolls that journal back; it never
infers a retry from an in-progress marker. Recovery refuses unknown edits made
after the interruption.

When the plan classifies `AGENTS.md`, `CLAUDE.md`, or `.aethyme/gates.toml`
as customized, `--resolution-file` accepts a schema-1 JSON object mapping each
reported path to `preserve`, `merge`, or `replace`. The same resolution file
must be supplied to `apply`; the choices participate in the confirmed digest.
Marked Markdown and `.gitignore` blocks preserve surrounding content, while
gate merges use the typed TOML migration and retain comments and custom gates.
No policy file is implicitly force-replaced.

Canonical deployments track `.aethyme/repository.json`; local-only deployments
keep `.aethyme/local/repository.json` ignored. Broker commands fail closed in
an enrolled repository whose marker is missing, incomplete, or newer than the
binary. See the [repository upgrade guide](../guides/repository-upgrades.md)
for review, interruption, and rollback behavior.

## Broker Commands

The broker is the stable product front door for multi-agent coordination:

For task-oriented examples that connect session reuse, gate cache policy,
lease planning, and durable finish handoffs, see the
[broker follow-up workflows guide](../guides/broker-workflows.md).

`broker worktree-root` is a strictly read-only placement plan. It reports the
canonical checkout identity, the preferred private host-state root, whether
that root is outside the repository, and the legacy fallback. Normal starts
use a clone-specific key derived from the canonical Git common directory, so
same-named independent clones do not share worktrees. macOS uses
`~/Library/Application Support/Aethyme/worktrees/`; other supported Unix hosts
use their configured Aethyme/XDG state directory. Set `AETHYME_WORKTREE_ROOT`
to choose another external base. The broker appends the clone key, writes a
private ownership marker, and refuses a root inside this repository or any
linked worktree.

If the platform host-state root cannot be prepared, `broker start` uses the
legacy `.aethyme/worktrees/` location and reports the exact fallback reason in
text and JSON. Explicit environment overrides fail closed instead of silently
falling back. Existing legacy sessions remain cleanup-compatible.

- `aethyme init`
- `aethyme certify`
- `aethyme broker status [--json]`
- `aethyme broker worktree-root [--json]`
- `aethyme broker start --task "..." [--path <repo-path>]... [--json]`
- `aethyme broker adopt [<path>] --task "..." [--path <repo-path>]... [--reuse [--sync-integration]] [--json]`
- `aethyme broker prepare status --session <id> [--json]`
- `aethyme broker prepare --session <id> [--offline] [--wait <duration>] [--json]`
- `aethyme broker exec --session <id> -- <command> [--json]`
- `aethyme broker git --session <id> [--repo <owner/name>] [--scope <scope>] [--effect <read|write|destructive>] [--reason <text>] [--destructive] -- <git-args>`
- `aethyme broker gh --session <id> --repo <owner/name> [--scope <scope>] [--effect <read|write|destructive>] [--reason <text>] [--destructive] -- <gh-args>`
- `aethyme broker operations list [--limit <n>] [--before <id>] [--session <id>] [--status <status>] [--repo <canonical-id>] [--provider <git|github>] [--json]`
- `aethyme broker operations [same options]` (compatibility alias during deprecation)
- `aethyme broker operations show <id> [--json]`
- `aethyme broker operations reconcile --operation <id> --outcome <succeeded|failed> --reason <text> [--json]`
- `aethyme broker advisories list [--all] [--json]`
- `aethyme broker advisories show <id> [--json]`
- `aethyme broker advisories ack <id> [--json]`
- `aethyme broker advisories metrics [--json]`
- `aethyme broker external-events ingest <normalized.json> [--json]`
- `aethyme broker external-events list [--all] [--json]`
- `aethyme broker external-events show <id> [--json]`
- `aethyme broker external-events reconcile <id> --outcome <assign|ignore> --reason <text> [--session <id>] [--json]`
- `aethyme broker review register --session <id> --repo <owner/name> --pr <number> [--json]`
- `aethyme broker review show --session <id> [--json]`
- `aethyme broker review request --session <id> [--json]`
- `aethyme broker review unlock --session <id> [--json]`
- `aethyme broker review reassign --session <closed-id> --to-session <live-id> --reason <text> [--json]`
- `aethyme broker review abandon --session <id> --reason <text> [--json]`
- `aethyme broker queue history [--limit <n>] [--before <id>] [--json]`
- `aethyme broker queue [--json]` (compatibility full-inventory view)
- `aethyme broker exposures plan [--json]`
- `aethyme broker exposures apply --session <id> --confirm <sha256> [--json]`
- `aethyme broker note send --session <sender> --to-session <recipient> --message <text> [--json]`
- `aethyme broker note list --session <recipient> [--json]`
- `aethyme broker note ack --session <recipient> --id <note-id> [--json]`

Repository-owned dependency preparation is optional and declarative. When
`.aethyme/prepare.toml` exists, `start` and `adopt` return a structured
`preparation` status but never execute it. `prepare status` is also strictly
read-only: it hashes the declaration, exact input bytes, OS, architecture, and
metadata for each declared runtime executable without launching repository
commands or making network requests. The explicit `prepare` command is the
only execution boundary.

```toml
schema_version = 1

[[runtimes]]
name = "node"
command = ["node", "--version"]

[[steps]]
name = "javascript-dependencies"
command = ["pnpm", "install", "--frozen-lockfile"]
offline_command = ["pnpm", "install", "--offline", "--frozen-lockfile"]
inputs = ["package.json", "pnpm-lock.yaml"]
outputs = ["node_modules/"]
cache = "repository_shared"
required_for_hooks = true
```

Commands are argv arrays, not shell strings; Aethyme does not infer npm,
pnpm, Yarn, Cargo, Python, or any other ecosystem. Every input and output is a
validated repository-relative path. Outputs remain inside the worktree and a
symlink at an output path is refused. Repositories should ignore local outputs
in Git. `worktree_local` steps receive no shared cache. A
`repository_shared` step receives `AETHYME_PREPARE_CACHE_DIR`; access is
serialized in per-user host state by canonical remote identity, renewed while
the command runs, and released afterward. The cache path and ownership token
are absent from JSON and broker journals.

The digest changes with command definitions, input bytes, platform,
architecture, or declared runtime executable identity. State is one of
`not_configured`, `required`, `current`, `stale`, `in_progress`, `failed`, or
`invalid`. Failed and interrupted state includes bounded evidence and the
exact retry command. `--offline` uses only explicit `offline_command` values
and refuses the entire run before execution when any step lacks one.

Preparation commands run through the broker's worktree guard. Dirty work that
already exists is preserved; newly changed tracked paths still require the
session's explicit leases. Hooks block only when a step sets
`required_for_hooks = true`, keep staged changes intact, and print the exact
`broker prepare --session <id>` remediation. Contributors without local
broker activation retain the existing no-op behavior.

Status is a bounded present-state view. Text and JSON expose live, pending, and
conflicted merge-queue entries individually, while `queue_history` contains a
versioned terminal-count summary and the exact history command. Use
`aethyme broker queue history` for a stable newest-first page; `--before`
advances without duplicating the boundary row, and `next_before_id: null`
proves the end. The bare `broker queue` command remains a documented
compatibility full-inventory view, but status no longer loads terminal rows.

Text status orders remediation by urgency: summary and warnings, outstanding
advisories and publication exposures with exact inspection commands, live
sessions and current queue entries, then terminal-history counts. Advisory and
exposure detail is capped at ten rows in text; their authoritative inventories
remain available through the printed commands. Cleanup retention fields report
retained worktree and session-branch counts, eligible count, retained and
reclaimable bytes, oldest closed-session age, the configured age policy, and a
typed severity. Severity becomes `warning` when retained count reaches five,
estimated retained bytes reaches 1 GiB, or the oldest closed worktree reaches
the configured retention age; otherwise it is `notice`.

Exposure reconciliation is explicit because normal status is non-mutating.
The plan queries the remote default branch without updating local refs and
binds its exact SHA, candidates, and live-lease blockers into a digest. Apply
rebuilds that plan and records a second coordinated remote observation before
resolving local lifecycle rows. Stale tracking refs are reported but are not
used as publication authority; a missing remote commit object blocks the plan
until an explicit fetch makes ancestry verification possible.
- `aethyme broker resources plan <request.json> [--json]`
- `aethyme broker resources acquire <request.json> [--wait <duration>] [--grant-out <path>] [--json]`
- `aethyme broker resources run <request.json> [--wait <duration>] [--cleanup-command <shell>] [--json] -- <command> ...`
- `aethyme broker resources renew <grant.json> --ttl <seconds> [--json]`
- `aethyme broker resources release <grant.json> [--json]`
- `aethyme broker resources list [--all] [--json]`
- `aethyme broker resources reconcile <lease-id> --confirm <generation> [--json]`
- `aethyme broker gates validate [--json]`
- `aethyme broker gates manifest [--head <ref>] [--json]`
- `aethyme broker gates scope --base <ref> --head <ref> [--json]`
- `aethyme broker gates affected --session <id> [--why] [--json]`
- `aethyme broker gates semantic --session <id> [--json]`
- `aethyme broker gates run --session <id> [--only <gate>] [--no-cache] [--json]`
- `aethyme broker gates run --all [--only <gate>] [--no-cache] [--json]`

Repositories that commit `.aethyme/graph/**` as authoritative generated state
must opt in explicitly:

```toml
[graph]
authority = "committed_fragments"
repository = "owner/repository"
```

For that mode, session gates, named reruns, `gates run --all`, the
repository-owned pre-push adapter, and submit-time merged-tree verification all
regenerate fragments in a locked disposable checkout before running configured
gates. A stale, deleted, corrupted, or wrong-version fragment set refuses with
the full exact-tree and policy digests plus repository-relative changed paths.
The caller worktree and index are never rewritten. `.aethyme/graph_store.redb`
is derived local state and is deliberately not part of this authority check.
Repositories without this declaration retain normal gate behavior.

`--only <gate>` runs exactly one configured gate, regardless of its path
triggers, for focused diagnosis after a failure. Text-mode failures replay the
last 20 captured output lines (bounded to 16 KiB); JSON remains structured and
continues to expose the complete local `log_path` without embedding log data.
- `aethyme broker gates pre-push <remote-name> [<remote-url>] [--no-cache] [--json]`
- `aethyme broker hooks install [--json]`
- `aethyme broker hooks uninstall [--json]`
- `aethyme broker hooks status [--json]`
- `aethyme broker leases plan <paths...> [--session <id>] [--json]`
- `aethyme broker leases export (--session <id> | --entry <id>) [--limit <n>] [--json]`
- `aethyme broker submit --session <id> [--no-cache] [--json]`
- `aethyme broker repair --session <id> [--json]`
- `aethyme broker finish --session <id> [--json]`
- `aethyme broker cleanup <session-id> [--force] [--json]`
- `aethyme broker cleanup --all-cleaned [--apply --confirm <sha256>] [--json]`
- `aethyme broker gc plan [--json]`
- `aethyme broker gc apply --confirm <sha256> [--json]`
- `aethyme broker handoff (--session <id> | --worktree <path>) [--json]`
- `aethyme broker report capture --kind <bug|improvement> --title <text> [--session <id>] [--include-task] [--stdout | --output <filename>] [--json]`
- `aethyme broker report list [--json]`
- `aethyme broker report show <filename> [--json]`
- `aethyme broker report render <filename> --form <form.yml> [--output <name>.issue.md] [--json]`
- `aethyme broker report file <path> --repo <owner/name> --confirm <sha256> [--json]`
- `aethyme broker ship plan --entry <id> [--json]`
- `aethyme broker ship execute --entry <id> --confirm <full-integration-sha> [--sync-main] [--break-glass --reason <authorization>] [--json]`
- `aethyme broker integration status [--json]`
- `aethyme broker integration reconcile --upstream <ref> [--resolution-file <path>] [--write-resolution-template <path>] [--dry-run|--apply --confirm <sha256>] [--json]`
- `aethyme broker quick-test [--with-gate] [--json]`
- `aethyme broker verify-loop [--json]`
- `aethyme broker pr check [--target <branch>] [--pr <number>] [--agent <name>] [--dispatch] [--cmd <command>] [--json]`

`quick-test` is the disposable install smoke. `verify-loop` is the stronger
operator E2E: it reports the integration commit tested and flags movement during
the run, so callers know whether the result proves the current integration tip.

`broker operations list` reads the durable operation journal newest-first. The
default page size is 50 and `--limit` accepts 1 through 500. Filters combine
with AND semantics; `--repo` matches the persisted canonical coordination ID
exactly. JSON is a stable page object:

```json
{
  "operations": [],
  "next_before_id": null
}
```

When `next_before_id` is non-null, pass it unchanged as `--before`; the cursor
is exclusive, so adjacent pages do not duplicate the boundary row. A null
cursor proves that no older matching row remains. The bare `broker operations`
spelling is retained as an alias for `broker operations list` during its
deprecation window.

`broker operations show <id>` returns the exact durable row plus a typed
reconciliation view. The view distinguishes `not_required`, `required`,
`reconciled_succeeded`, and `reconciled_failed`; includes preserved exact-push
evidence when available; and renders both complete reconciliation commands for
an unknown outcome. `automatic_retry_allowed` is always false. A second clone
remains blocked by the host-wide unknown-outcome barrier until an operator
inspects external state and runs one explicit reconciliation command.

Every `operations reconcile` usage or validation error repeats the complete
contract—`--operation`, `--outcome`, and `--reason`—in one message. Successful
manual reconciliation appends the operator outcome and reason without deleting
the original push plan or remote evidence.

`broker advisories` exposes durable, explicitly non-blocking findings. Each row
has an immutable producer identity, optional session and queue-entry links,
severity, the exact integration SHA when relevant, repository-relative paths,
structured evidence, creation time, and a typed `outstanding`, `acknowledged`,
or publication-`resolved` state. `list` returns outstanding rows newest-first;
`list --all` includes terminal history, and `show <id>` returns one exact row.
`ack <id>` is idempotent and preserves the row and evidence.

`advisories metrics` exposes bounded shown-to-action correlation as text or a
versioned JSON object. One row is retained per advisory and delivery surface;
repeated delivery increments its count rather than appending history. The
allowlist is limited to advisory and session IDs, surface, first/last display
times, count, action time, and acknowledged/publication-resolved action. It
does not store task text, command arguments, paths, evidence, diffs, or secrets.
The metrics are operational feedback only and never change advisory severity
or broker behavior.

After each advisory creation or acknowledgement, the broker takes a
cross-process projection lock, re-reads outstanding rows from SQLite, and
atomically replaces `.aethyme/broker-advisory.md`. That generated, gitignored
file is a convenient persistent summary only: ignored files are not
automatically visible to agents, `.aethyme/broker.db` remains authoritative,
and advisories never select gates or block submit, promotion, or shipping.
Managed post-commit output, common session-associated broker commands, and the
pre-expensive-gate boundary deliver outstanding notices. Generated AGENTS and
CLAUDE guidance tells agents to inspect `broker status --json` when a notice
appears and after rebase or worktree reuse, then read the projection when a
delivery surface points to it.

`broker external-events` is the bounded handoff from authenticated provider
adapters into that advisory model. Aethyme does not run a webhook listener or
poll a provider in the background. The adapter must first authenticate its
source, then write one strict schema-1 JSON envelope containing only the
provider event ID, supported event type, canonical repository, target branch,
pull-request number, exact full commit SHA, event and verification times,
verification method, and a SHA-256 over those normalized fields. Payloads,
comments, review bodies, diffs, credentials, and arbitrary metadata are
rejected rather than retained. Input must be a regular, non-symlink file no
larger than 64 KiB.

The supported event types are `review-changes-requested`, `review-approved`,
`queue-ejected`, and `validation-failed`. Resolution requires an existing PR
watch and exact durable session or queue provenance for the named commit;
rewritten and closed sessions remain discoverable through those records.
Provider plus event ID is the deduplication key. Redelivery with the same
normalized digest is idempotent, while a changed digest is refused.

Unknown event types, unknown PRs, repository mismatches, stale deliveries,
missing owners, and ambiguous owners are retained as typed unresolved records
and never guessed into a session. Inspect them with `list` and `show`, then
either assign a verified session or ignore the event explicitly. Reconciliation
stores only a digest of the operator reason. Every resulting advisory is
ordinary and non-blocking: it cannot select gates, stop promotion, or authorize
publication.

When a verified promotion changes a path covered by another live session's
explicit or implicit lease, the broker records one deterministic warning for
that affected session. It binds the originating queue entry, exact 40-character
integration SHA, intersecting promoted paths, lease evidence, and the safe next
command. Promotion remains non-blocking and the broker never rebases or edits
the affected worktree. Outstanding notices appear on stderr for every broker
command associated with that session, after managed post-commit output, and
immediately before an uncached gate with cost greater than 1 starts. JSON stdout
remains parseable. `broker status --json` includes both
`outstanding_advisories` and the authoritative
`outstanding_entry_exposures`; text status summarizes each exposed queue entry.
Acknowledgement stops future session notices without deleting history or
changing the underlying publication exposure.

`broker note` is a deliberately small, repository-local coordination channel
between live sessions. Messages are trimmed, limited to 1,000 UTF-8 bytes, and
must be a single line without control characters. The recipient sees unread
notes on stderr at the next session-associated broker command, while JSON
stdout remains parseable. `note list` returns newest-first durable history and
an `unread_count`; only the named recipient may acknowledge a note. Closing a
session prevents new sends to or from it.

Note text remains only in the repository's broker database. The append-only
`session.note.sent` and `session.note.acknowledged` events contain routing IDs,
timestamps, and the sent byte count, never message text. Notes are for terse
handoffs such as “coordinate `src/router.rs` before editing”; they do not claim
paths, refresh leases, alter gates, or replace task descriptions.

Every promotion also creates one authoritative path exposure owned by its
queue entry. It contains the exact promoted SHA and repository-relative path
set, survives closure of either the promoting or affected session, and is not
cleared by a worktree rebase. The broker resolves it—and any still-outstanding
advisory linked to that entry—only after `broker ship execute` observes a
remote-main SHA containing the promotion, or a confirmed `integration
reconcile --apply` proves an exact, patch-equivalent, or reviewed superseding
landing. Read-only plans, stale remote state, failed verification, ambiguous
outcomes, and pending replays retain the exposure.
If publication targets a selected integration prefix, containment in the
verified remote tip is still the resolution authority: included entries clear
in deterministic queue order, while later or otherwise non-contained entries
remain exposed. Directory-lease descendants are matched like exact paths;
duplicate exact, directory, and implicit overlap evidence produces one sorted
path set per affected session. Acknowledgement changes notice delivery only.
On the first normal open after the storage upgrade, currently promoted legacy
entries are backfilled from their exact first-parent commit deltas; diagnostic
snapshot opens remain non-mutating.

`broker gates pre-push` remains an opt-in full-gate adapter for repositories
that wire it into their own hook manager. It reads Git's ref-update lines from
stdin, requires all non-deletion updates to name one clean checked-out `HEAD`,
and runs the complete gate set. This makes the reported tree truthful and lets
declared host resources coordinate concurrent clones. See
[Concurrent Host Resource Coordination](../guides/host-resource-coordination.md)
for the gate schema, repository-independent supervised runs, hook example,
fallback contract, and quarantine recovery.

`broker adopt --reuse --sync-integration` starts a follow-up from the current
integration tip. It requires a clean session worktree, permits only a
fast-forward, and synchronizes before recording the follow-up diff baseline;
dirty or diverged worktrees are left unchanged.

Plain `broker adopt --reuse` preserves a live session's recorded ownership
baseline. Reuse may update its task and activity, but cannot absorb pending
commits into a new baseline. Close the completed session before adopting a new
identity when a genuinely fresh ownership boundary is required.

Closed sessions remain available to diagnostic reads, including `status`,
`handoff`, and `review show`. They cannot claim leases or run review mutations.
If a closed session still owns a review lifecycle, either reassign it to a live
session at the exact lifecycle commit or abandon it explicitly. Both commands
require a reason, retain the lifecycle audit history, and persist only the
reason digest.

`broker submit` builds a normalized commit-provenance plan before gate
selection. It replays only pending `session_owned` single-parent patches onto
the exact integration tip, in order. Patch-equivalent history already present
under another SHA is classified as
`already_integrated_by_stable_patch_identity` and is not replayed. Missing
baselines, ambiguous ownership or patch identity, and pending owned merge
commits are refused rather than guessed.

If normalized replay produces the same tree as integration, submit reports a
`superseded` queue entry with `no_changes: true`. It does not run gates, create
an empty promotion commit, or move integration. Primary-checkout sessions keep
their existing follows-main verification behavior because their work is
already externally present on `main` before submit records it.

With `--json`, `submission_plan` exposes the full recorded baseline, session
HEAD, integration HEAD, ordered commits, their parents, ownership,
integration state, stable patch ID, matching integration commits, safety flag,
and warnings. On rejection, `conflict_details` supplements the compatible
`conflicts` path list with the full originating commit, ownership, known
integration-side commits, remediation text, and ordered commands. A blocking
session is reported only when its current active lease overlaps a surviving
replay conflict.

`broker checkpoint plan --session <id> --json` exposes stable
`refusal_codes` and ordered `next_actions`. A safe plan can be applied only by
rebuilding it and confirming its digest with `checkpoint apply`. An unsafe
plan begins by preserving the exact session tip and directs the operator to
inspect and replay pending commits from a clean session. It never recommends a
blanket rebase onto integration. `broker repair` applies only to a recorded
submit or promoted-path conflict; otherwise it refuses immediately and points
to the checkpoint planner.

Gate-run and submit outcomes identify the exact Git tree each result proves.
Human-readable output abbreviates the tree hash to 12 characters; JSON retains
the full hash in `tree_hash` for both executed and cached results.
Pass `--no-cache` to either gate-run form or submit to require fresh gate
execution. Bypass skips cache lookup only: the fresh result is stored normally
and is available to a subsequent run using the default cache policy.

`broker gates manifest` is the portable, content-free policy export for CI and
merge-queue consumers. It reads `.aethyme/gates.toml` from the exact committed
`--head` (default `HEAD`), not from dirty or ignored checkout content. JSON
contains the normalized triggers, cost, cache policy, resource requirements,
managed-cache policy, opaque execution-definition hash, semantic-advice bounds,
and graph-integrity authority, policy digest, and checker version. Schema 2
introduced graph-integrity provenance; the manifest SHA-256 binds every field.
It never contains gate commands,
environment values, credentials, diffs, or absolute paths. Consumers must
reject unsupported schema versions, unknown fields, and digest drift.

`broker gates scope` evaluates that same committed policy with the same
`select_gates` implementation used by broker execution. It resolves both refs
to full commit SHAs and returns sorted repository-relative changed paths,
selected gates, first triggering path, and reason. Rename detection is
deliberately disabled for the path inventory so both old and new trigger
surfaces remain visible; deletions, binary files, and empty commits retain
deterministic behavior. The exact evaluator does not consult a local graph:
semantic suggestions are explicitly reported as advisory, unenforced, and not
included.

`broker gates semantic` is a separate, strictly advisory read surface:

```bash
aethyme broker gates affected --session 111 --why
aethyme broker gates semantic --session 111
aethyme broker gates semantic --session 111 --json
```

The first command reports the path-triggered gates that `gates run` and
`submit` enforce. The semantic command reports that same set plus optional
caller-frontier suggestions; it does not run gates, change gate configuration,
or add suggestions to either enforcement path.

For a warm graph, the broker maps each changed repository-relative file to its
indexed callable nodes, walks incoming `Calls` edges breadth-first, maps caller
nodes back to files, and matches those files against gate triggers. Gates
already selected by the changed paths are excluded. Each remaining suggestion
can carry an auditable chain:

```text
src/core.rs -> src/service.rs -> service-integration
```

Traversal is deterministic and strictly bounded: changed files and caller
adjacency are sorted, first-seen breadth-first order is preserved, depth is
limited to 2 call edges, at most 128 callable nodes are visited, and at most 64
caller paths enter one report. `truncated: true` means one of those bounds (or
provider-result sanitization) omitted additional candidates; it never causes
the broker to enforce the returned subset.

| Graph condition | Semantic status | Result |
| --- | --- | --- |
| Warm, with matching callers | `ready` | Returns caller paths, suggestion chains, frontier counts, and matching advisory gates. |
| Warm, but no changed-file callables or callers | `ready` | Returns an empty suggestion list; path-selected gates are unchanged. |
| Cold/missing redb store | `graph_missing` | Returns a successful report explaining that semantic suggestions are unavailable. |
| Stale store (committed graph fragments are newer) | `graph_stale` | Returns a successful report with no semantic suggestions and asks for a graph rebuild. |
| Corrupted, unreadable, or query-failing store | `provider_error` | Returns a successful report with the provider diagnosis; gate operations remain available. |
| Any warm lookup exceeding a bound | `ready` with `truncated: true` | Returns only the deterministic bounded prefix as advice. |

JSON keeps enforced and advisory data visibly separate in
`path_selected_gates` and `semantic_suggested_gates`. The nested `semantic`
object includes provider status and reason, full caller chains with depth,
configured result/depth/node limits, visited-node count, and truncation state.
Suggestion entries include the explainable changed-file → caller-file → gate
chain when the graph provider supplied one.

`broker hooks install` installs shared pre-commit, post-commit, and pre-push
shims. The
pre-commit hook runs matching cost-1 gates against the staged change and stays
silent when they pass. If a gate fails, the hook replays its complete standard
output and error, prints the broker diagnosis, returns the gate's non-zero exit
code, and blocks the commit. The one-shot Git escape hatch remains
`git commit --no-verify`.

The managed pre-push shim is a publication guard, not the full-gate adapter.
From an enrolled Git common directory it rejects updates to `main`, `master`,
the advertised origin default branch, and `aethyme/integration` unless Git is
running inside a broker-coordinated operation. Normal publication therefore
uses `broker ship plan` followed by digest/SHA-confirmed `broker ship execute`;
an explicitly authorized exceptional push uses `broker git`.

For emergency recovery only, set
`AETHYME_BROKER_BREAK_GLASS_REASON="<reviewed reason>"` on the one Git push.
The hook records protected refs, reason byte count, and a SHA-256 reason digest
in a local event, never the reason text. This is a cooperative local safety
boundary—`--no-verify` still exists in Git—but the safe and exceptional paths
are now explicit and auditable.

`broker leases plan` is a read-only preflight for files or trailing-slash
directory claims. It reports exact and directory overlaps with each active
lease's owning session, implicit or explicit kind, and expiry. Supplying
`--session` separates leases already owned by that session from foreign
conflicts; without it, every overlap is a potential conflict. Planning neither
claims nor refreshes leases and does not append broker events or command
telemetry. Paths are sorted deterministically and must be unambiguous,
repository-relative spellings without `.` or `..` components.

`broker leases export` is the stable integration boundary for external
path-scoped queues. It selects one session directly or through a merge-queue
entry and returns schema version 1 with a source timestamp, credential-free
canonical repository identity, and at most 200 lease rows by default (1,000
maximum). Every row distinguishes exact/directory, implicit/explicit,
active/expired/released/inactive-owner, and non-conflicting/overlapping state.
Truncated output reports both the selected limit and total matching rows.

Routing is explicit repository policy in the committed `.aethyme/config.toml`:

```toml
[leases.routing]
backend = ["backend/", "db/schema.sql"]
frontend = ["frontend/"]
```

Category and path ordering is deterministic. Directory spellings end in `/`;
the same exact/directory overlap rules used by lease claims determine category
membership. The export reads config from the exact committed main-checkout
HEAD, so dirty or ignored local files cannot change an adapter decision.

The JSON allowlist contains no remote URLs, credentials, task text, worktree or
host paths, commands, diffs, or ownership tokens. Export opens a read-only
broker snapshot: it does not refresh or extend leases, append events, or write
command telemetry. GitHub-label and merge-queue adapters should retry this
read-only projection and treat their own delivery as idempotent; they must not
write adapter state into the lease registry.

`broker start` and `broker adopt` accept repeatable `--path` declarations for
work known before a diff exists. The broker validates the complete normalized
set first, then commits session creation or reuse and every accepted path as an
ordinary explicit lease in one transaction. One exact or directory conflict
refuses the whole operation: no partial claims, task retargeting, or leftover
managed worktree. Reuse preserves an already-active owned lease without
silently extending its expiry; expired or released owned claims are
reactivated. Start/adopt text and JSON return the deterministic planned lease
set, and `broker status` includes all active leases so other agents can inspect
intent alongside task text before either session creates a diff.

`broker finish` returns a structured handoff covering the latest queue and
submitted/promoted/published delivery state, pending work, every recorded
active/released/expired lease, the latest executed or cache-resolved gate with
its full tree hash and event time, cleanup safety, physical cleanup outcome,
and one recommended next action. A successful finish closes broker state first,
then reclaims a represented broker-owned spawned worktree and its exact checked
branch by default. It reports exact reclaimed bytes and whether each artifact
was removed. Use `--keep-worktree` to close the session without physical
cleanup; `broker close` also remains state-only.

The redacted `session.finished` handoff survives both state closure and physical
cleanup. If cleanup stops part-way, the session remains closed, retained
artifacts remain represented, and the report gives the exact
`broker cleanup <session-id>` recovery action. Running `finish` again resumes
that cleanup idempotently. Refused finishes do not emit a misleading handoff.

`broker cleanup <session-id>` explicitly removes one exact session worktree
after the same safety checks. `broker cleanup --all-cleaned` is a read-only
bulk plan by default. It classifies each retained worktree and branch as represented,
pending, or unproven from the accepted session checkpoint, queue entry, promoted
integration commit/tree, and current delivery refs. The plan includes exact
inspection commands, byte estimates, branch tips, and a SHA-256 digest. Apply
the reviewed plan with `--apply --confirm <sha256>`; apply rebuilds the plan and
revalidates every candidate before removing the worktree and its exact checked
session branch. An interruption after worktree removal leaves the branch in the
next plan for safe recovery. Adopted worktrees are outside the sweep, and dirty,
symlinked, unsafe-path, pending, unproven, or inspection-failed candidates remain
untouched. `--force` is available only for one exact session and is rejected
with `--all-cleaned`; there is no blanket discard authorization.

`broker gc` applies one declared retention policy across terminal events,
gate results and their broker-owned logs, terminal merge-queue history,
command metrics, closed represented worktrees, build caches inside retained
worktrees, and orphaned host worktree roots. The optional
`.aethyme/broker.toml` file overrides conservative defaults:

```toml
[retention]
schema_version = 1
terminal_events_days = 180
gate_results_days = 30
terminal_merge_queue_days = 180
command_metrics_days = 30
closed_worktrees_days = 7
retained_bytes_budget = 1073741824
artifact_reclaim_days = 0
orphan_worktree_roots_days = 1
artifact_sweep_budget_ms = 5000
artifact_sweep_interval_hours = 24
startup_budget_ms = 25
```

`retained_bytes_budget` is a soft, non-blocking budget used by status, doctor,
and finish warnings; `0` disables only those warnings. It never authorizes
deletion. `artifact_reclaim_days` and `orphan_worktree_roots_days` accept `0`,
meaning no grace period. Build caches from closed sessions have no grace by
default: preserving a contribution must not also retain multi-gigabyte derived
outputs. Raise `artifact_reclaim_days` to trade disk space for faster worktree
reuse, or set `artifact_sweep_budget_ms = 0` to disable autonomous cache
reclamation entirely.

Run `aethyme broker gc plan` first. Its text and stable JSON enumerate every
eligible database row, runtime file, represented worktree and exact branch ref,
build cache, orphaned root, estimated bytes, protected finding, and the SHA-256
authorization digest. GC never ages out live sessions, outstanding or
acknowledged advisories, unpublished exposures, unresolved coordinated
operations, accepted checkpoints, or unproven contributions.

The plan also reports `estimated_retained_bytes` and `estimated_blocked_bytes`
alongside `estimated_reclaimable_bytes`, so it states total disk pressure rather
than only the bytes this plan will act on. The two reporting totals are excluded
from the authorization digest: a measured size change must never invalidate a
plan an operator already confirmed.

### Build caches and orphaned roots

A blocked cleanup disposition protects committed work. A git-ignored build cache
holds none and is recovered by rebuilding, so build caches are reclaimed
independently of the worktree's disposition: a worktree whose provenance is
unproven still has reclaimable bytes. A directory qualifies only when its name is
recognised (`target`, `node_modules`) *and* a witness confirms it is a real cache
— `CACHEDIR.TAG` for `target`, a non-empty directory for `node_modules` — so a
source directory that merely shares the name is never removed. The scan is
depth-bounded, skips git metadata, and never follows symlinks.

By default, build caches from sessions idle for `artifact_reclaim_days` are
reclaimed automatically on broker startup, without per-run confirmation.
This is the one exemption from the rule that startup never authorizes fresh work,
and it is narrow: the sweep removes only build caches, never commits, refs, or
worktrees, and it skips live sessions. It runs at most once per
`artifact_sweep_interval_hours` within `artifact_sweep_budget_ms`, so discovery
stays off the hot path of every broker command. If the budget expires before a
backlog is scanned, the next broker command resumes maintenance instead of
holding the remaining caches for a full interval. A later broker startup
continues that bounded backlog for the repository. Its cadence stamp lives in the
broker database rather than under `.aethyme/`, where a durable runtime file would
register as a dirty path to the checkout-cleanliness gates. A sweep with no
closed worktree does not consume the cadence window, so the first command after
a session closes can reclaim its caches. With an explicitly disabled policy, or
to clear an existing backlog immediately, use `gc plan` and digest-confirmed
`gc apply`.

Worktree storage is host-scoped while the records that own it are
repository-local, so a deleted repository leaves a worktree tree that no database
can account for again. Each host worktree root carries a
`.aethyme-worktree-root.json` breadcrumb naming its owning repository; when that
repository no longer exists the root becomes reclaimable. A root with no readable
breadcrumb is reported as a blocker and never removed blind, and an authorization
stops binding the moment the owning repository reappears. Repositories under the
system temporary directory are never anchored in the implicit platform host-state
directory for this reason, though an explicitly configured
`AETHYME_HOST_STATE_DIR` or `AETHYME_WORKTREE_ROOT` is always honoured.

Apply only the reviewed plan:

```bash
aethyme broker gc apply --confirm <sha256>
```

Application uses an exclusive stale-owner-aware lock, transactional row
batches, exact before-hash checks for files, and provenance-safe worktree/ref
cleanup. Progress is journaled atomically in `.aethyme/gc-journal.json`.
Subsequent broker startup may spend only the configured monotonic time budget
resuming that already-confirmed journal; startup never authorizes a fresh plan.
If a file changed or remains locked, it is retained and the exact recovery
command remains available. Event IDs and operation cursors are not reused.
`broker doctor` and `aethyme certify` report policy validity, eligible counts,
bytes, blockers, and pending recovery without embedding retained content.

Retrieve the newest persisted handoff without changing broker state:

```bash
aethyme broker handoff --session 110
aethyme broker handoff --worktree /path/to/former-session-worktree --json
```

Exactly one selector is required. Session lookup returns that session's latest
`session.finished` event. Worktree lookup considers cleaned sessions too and
returns the newest completed handoff registered to the exact worktree path,
including when the worktree has since been removed and its former absolute path
is supplied. JSON adds the handoff event's stable `event_id` and `recorded_at`
provenance without exposing the worktree path. The command does not refresh or
append sessions, leases, gates, events, or command telemetry.

Capture a reviewable diagnostic report without contacting Git remotes,
GitHub, the graph store, or gate runners:

For the end-to-end security workflow, private-repository warnings, and a
redacted example, see [Capture And File Broker Reports Safely](../guides/report-capture.md).

```bash
aethyme broker report capture --kind bug --title "Submit gate failed"
aethyme broker report capture --kind improvement \
  --title "Explain cache misses" --output reviewed.json
aethyme broker report capture --kind bug --title "Pipe this report" --stdout
```

The default and `--output` forms publish a new JSON file atomically beneath
`.aethyme/reports/`; initialized repositories ignore that directory. An
explicit output may be a filename or `.aethyme/reports/<filename>`, cannot
escape the report directory, and never overwrites an existing artifact.
`--stdout` emits the exact JSON bytes on standard output and puts the digest
on standard error so pipelines remain clean. Every mode prints the SHA-256 of
the exact report bytes for later review confirmation.

When `--session` is omitted, capture uses the broker session registered for
the current Git worktree when one exists. The snapshot remains allowlist-only:
file contents, diffs, hunks, command arguments, logs, absolute paths, and event
payloads are excluded. Task text and coordinated-operation authorization
reasons appear only with explicit `--include-task`. `--stdout` and `--json`
are intentionally mutually exclusive because stdout is already the report's
JSON byte stream.

Inspect the local report inventory without changing broker or report state:

```bash
aethyme broker report list
aethyme broker report list --json
aethyme broker report show reviewed.json
aethyme broker report show .aethyme/reports/reviewed.json --json
```

List ordering is deterministic: valid reports are newest-first by
`captured_at`, then by repository-relative path; invalid entries are sorted by
path and do not hide valid artifacts. Both stable JSON surfaces include schema
version 1, the current exact-byte SHA-256 digest, capture time, report kind,
capturing Aethyme version, and `filed`/`unfiled` state. Show additionally
returns the parsed allowlist-only report document.

Filing state is read from the hidden local `.aethyme/reports/.filings.json`
index by digest, never by filename. A missing index means every report is
unfiled. Renaming a report preserves state; changing its bytes produces a new
digest and therefore returns it to unfiled. List reports corrupt, unsupported,
oversized, non-file, or symlinked artifacts in `invalid`; show fails closed
when its selected artifact has any of those conditions. Neither command opens
or creates the broker database, appends telemetry, or contacts external state.

Render a captured report into the repository's own GitHub issue-form order:

```bash
aethyme broker report render reviewed.json --form bug_report.yml
aethyme broker report render reviewed.json \
  --form .github/ISSUE_TEMPLATE/bug_report.yml --json
```

The form selector is confined to `.github/ISSUE_TEMPLATE/*.yml`; absolute
paths, traversal, nested paths, other extensions, symlinks, oversized files,
and malformed YAML fail locally. Rendering reads only the selected report and
form. It does not open broker state, execute Git, contact GitHub, or make any
network request.

Form `body` entries retain their declared order. Static `markdown` entries are
copied as repository-authored instructions. For `input`, `textarea`, and
`dropdown` entries, exact field IDs from this allowlist can consume captured
report data:

- `summary`, `problem`, `description`
- `kind`, `report_kind`, `digest`, `report_digest`
- `version`, `aethyme_version`, `environment`, `platform`
- `session`, `session_details`, `task`
- `failure`, `last_failure`, `logs`, `logs_or_output`
- `gates`, `gate_results`, `operations`, `recent_events`, `events`
- `diagnostics`, `report`, `report_snapshot`

Task text is available only when the report itself was captured with
`--include-task`. Dropdown values are filled only when a mapped value matches
one of the form's options case-insensitively. Checkboxes, unsupported controls,
unknown IDs, and known IDs without captured data render as explicit `Unfilled`
sections; they are never guessed or silently omitted. If any such field is
required, the command still emits the complete reviewable Markdown (or JSON)
and then exits non-zero with the missing IDs. Plain output writes only the
issue body to stdout, while the proposed issue title and report digest go to
stderr so redirection stays useful.

For a review-and-file workflow, write a human-editable Markdown artifact:

```bash
aethyme broker report render reviewed.json --form bug_report.yml \
  --output reviewed.issue.md
# Read and edit .aethyme/reports/reviewed.issue.md, replacing required
# Unfilled sections with the human-supplied answers.
shasum -a 256 .aethyme/reports/reviewed.issue.md
aethyme broker report file .aethyme/reports/reviewed.issue.md \
  --repo owner/name \
  --confirm <full-sha256-printed-above>
```

`--output` atomically creates, and never overwrites, an `.issue.md` file under
`.aethyme/reports/`. Its visible content is ordinary editable Markdown. A
hidden metadata comment preserves the proposed issue title, source report
digest, form path, and required-field contract. Reviewed `.issue.md` artifacts
are excluded from `report list`, which continues to inventory source captures.

`report file` accepts that artifact by filename or its exact
`.aethyme/reports/<filename>.issue.md` path. The full lowercase SHA-256 must
match the current artifact bytes; a post-review edit therefore fails before
`gh` is invoked. Filing also rechecks required sections and refuses any that
are absent, empty, or still contain the generated `Unfilled` marker.

The remote mutation runs as `gh issue create` through the existing coordinated
operation layer, using the current worktree's broker session and the exact
`--repo owner/name` target. Title and temporary body-file paths are redacted
from the command journal. On success, the returned issue URL and number are
validated against the requested repository, stored in the operation result,
and recorded by source-report digest in `.filings.json`; `report list/show`
then report that source capture as `filed`.

Any non-zero mutating command, unparseable success response, or failure to
persist the issue identity becomes `outcome_unknown`. The command prints the
operation ID, exits non-zero, and directs the operator to inspect GitHub and run
`broker operations reconcile`. A later `report file` does not retry while that
unknown repository operation remains unresolved. Reconciliation as `succeeded`
continues to block duplicate filing for that source report; reconciliation as
`failed` permits a later, separately confirmed filing command.

Promotion only advances the local integration ref. Use `broker ship plan` to
inspect the selected promoted-prefix SHA, the entries included through it,
later entries explicitly excluded from it, the current integration tip, remote
freshness, proposed non-force push, and local-main safety without mutating refs
or remote state. Publish only with the plan's full publication SHA:

```bash
aethyme broker ship plan --entry 42
aethyme broker ship execute --entry 42 \
  --confirm 0123456789abcdef0123456789abcdef01234567
```

Execution fetches and revalidates the planned remote base, requires a
fast-forward, pushes that exact selected-prefix SHA, and verifies the remote
default ref. A later integration promotion does not broaden the confirmed
publication. The planner refuses a prefix containing an integration commit
without promoted queue provenance. It
then resolves every outstanding entry exposure whose promotion is an ancestor
of that verified remote SHA; selecting a later promoted entry therefore closes
the verified published prefix without guessing about unrelated entries. It
leaves the primary checkout unchanged unless `--sync-main` is present; that
option additionally requires a clean, unchanged, fast-forwardable local
default branch. `broker integration status` reports whether the integration tip
is promoted, published, or locally synchronized and routes its next action
through this ship lane.

Publication authorization is independently opt-in. The default remains the
direct, full-SHA-confirmed lane above. A repository that requires reviewed
publication commits this policy before the promoted prefix is created:

```toml
[publication]
schema_version = 1
mode = "review_gated"
allow_break_glass = false
```

For `review_gated`, plan JSON contains a `publication_policy` assessment for
every included queue entry. Each entry must be covered by a
`validation_unlocked` lifecycle for the same canonical repository, default
branch, promoted session commit, and selected prefix. Execute re-fetches the
live PR and refuses an older head, changed base, closed or draft PR, dismissed
approval, provider outage, lifecycle drift, or partial prefix coverage. Review
evidence never broadens the confirmed SHA and does not bypass freshness,
fast-forward, unknown-outcome, or exact-push checks.

Emergency publication is unavailable unless the committed policy sets
`allow_break_glass = true`. It is then a separate explicit action:

```bash
aethyme broker ship execute --entry 42 \
  --confirm 0123456789abcdef0123456789abcdef01234567 \
  --break-glass --reason "incident authorization reference"
```

The reason must be supplied at execution time. Only its SHA-256 is retained in
the report and coordinated-operation journal; the text is not persisted.

Frozen broker JSON contracts are limited to the commands listed in
[`../../../../docs/json-contracts.md`](../../../../docs/json-contracts.md).
Other `--json` outputs are useful but provisional.

`broker git` and `broker gh` are the coordinated route for commands that can
affect shared refs or GitHub state. The executable is fixed (no shell), known
commands are classified as read, write, or destructive, and ambiguous commands
fail closed unless `--effect` and `--scope` are declared. Destructive commands
also require `--destructive`. Remote Git operations and every `gh` operation
require an exact `--repo owner/name` target.
Every write also requires a concise `--reason` identifying the user request or
documented workflow that authorized the state change.

Read-only `git`/`gh` inspection may run directly, as may explicitly authorized
local `gh auth` setup. The broker route is mandatory when a command can mutate
shared refs, GitHub repository/account state, or another session's surface.

Writes take a cross-process repository lock and write prepared/running/terminal
rows to the broker database. Command output is returned to the caller but never
persisted; content- and secret-bearing argument values are redacted from the
journal. If a process dies after starting, the next overlapping write marks the
operation `outcome_unknown` and refuses to run. Inspect external state, then use
`operations reconcile` to attest `succeeded` or `failed`; never retry an unknown
operation blindly. A non-zero write is also `outcome_unknown`, because a remote
command may apply only part of its requested change before failing. The broker
can resolve a non-zero `git push` more precisely when every refspec explicitly
names one non-deletion source and one fully-qualified destination
(`[+]source:refs/...`). Before execution it records each proposed object and
the exact destination's advertised SHA (or proven absence); afterward it
queries every destination without updating local tracking refs. All refs still
at their planned bases is a safe failure, while all refs at their proposed SHAs
is a reconciled success. Missing, unexpected, or mixed evidence remains
`outcome_unknown` (with mixed base/proposed refs recorded as `partial`). Pushes
using shorthand, deletion, wildcard, `--all`, `--mirror`, `--tags`, or another
unplannable shape remain conservatively unknown. Output text never participates
in this classification. V1 deliberately serializes all writes for one
repository.

After a successful coordinated `gh pr merge`, the broker performs a second,
journaled fetch of the primary branch's configured remote-tracking target. It
then removes stale integration history automatically only when the complete
recorded promotion layer is conclusively present upstream by ancestry, stable
patch identity (including a cumulative squash), or identical path content.
The cleanup uses the same crash-recoverable ref/database reconciliation as the
manual command. A queued auto-merge, failed fetch, local-main divergence,
unrecorded commit, unmatched promotion, or ambiguous result leaves integration
unchanged and reports the exact inspection or dry-run command. A successful PR
merge is never reported as failed merely because this follow-up cleanup was
deferred.

`integration reconcile` is the recovery path when main moves outside the
broker, including deploy-authored release commits and squash merges. It never
fetches: first update the remote-tracking ref through an authorized coordinated
Git operation, then run the default dry-run. The read-only plan enumerates
upstream-only external work, recorded promotions, exact and patch-equivalent
landings, unrecorded integration commits, pending queue entries, and ambiguous
equivalence using full SHAs.

Exact ancestry, stable patch IDs, and path-tree equivalence classify recorded
work. Remaining promotions and explicitly preserved unrecorded commits are
replayed in their original integration order onto the current upstream tip.
Ambiguous equivalence or a replay conflict blocks without changing refs or
broker rows. A safe dry-run prints `plan_digest`, a SHA-256 over the current
upstream/integration inputs, classifications, and reviewed dispositions.
`--apply` requires that exact digest with `--confirm`; ref drift or edited
review inputs therefore requires a new dry-run.

The confirmed update moves the integration ref and queue state together. A
durable two-phase intent includes the reviewed digest, so the next broker open
completes the queue/audit transaction if the ref moved, cancels the intent if
it did not, and refuses to guess if the ref has a third value.
Entries proven landed or superseded resolve their path exposures in that same
database transaction. Entries replayed onto the new integration tip retain the
same exposure with its promoted SHA retargeted to the replayed commit.

`broker status` and `broker integration status` run the conclusive portion of
this classifier without changing refs, queue rows, worktrees, or remote state.
JSON includes full promotion and upstream landing SHAs. A complete layer whose
entries are all landed is `reconciliation_ready` with notice severity; any
unresolved, ambiguous, incomplete, or unrecorded layer remains blocked and
uses the reviewed reconciliation workflow below.

When automatic evidence correctly fails closed because landed work was later
modified upstream, an operator can attest only the affected queue entries with
a versioned resolution file. Schema 2 also requires one explicit disposition
for every unrecorded integration SHA: `preserve_and_replay`,
`replaced_by_exact_upstream_sha`, or `drop_because_content_empty`. Replacement
must name one full SHA reachable from the bound upstream; dropping is accepted
only when Git proves the commit tree is unchanged. There is deliberately no
blanket discard option.

The first blocked dry-run includes a `resolution_template` object in JSON. Its
`document` is the complete schema-2 file: exact current refs, every unresolved
recorded and unrecorded identifier, and any valid attestations already loaded
from `--resolution-file`. New operator judgments and reasons are `null`, so the
document deliberately fails validation until reviewed. Sibling
`field_contract`, `recorded_evidence`, and `unrecorded_evidence` fields explain
allowed values, full-SHA rules, conflicts, matching commits, changed paths, and
whether Git proved a commit content-empty. Write only the ready-to-edit
document atomically, without overwriting an existing review file, with:

```bash
aethyme broker integration reconcile \
  --upstream origin/main \
  --write-resolution-template reconciliation.json \
  --dry-run
```

The file is single-use by construction: it binds the named ref's exact fetched
commit, the old integration tip, each queue ID and original promoted merge
commit, and each unrecorded integration SHA. Unknown fields, duplicate entries,
stale commits, empty reasons, invalid dispositions, and redundant overrides of
automatic matches are rejected before planning or mutation.

```json
{
  "schema_version": 2,
  "upstream_ref": "origin/main",
  "upstream_commit": "7033b70ec0241a9f01ab7ac5577dd74039b53e38",
  "old_integration": "3e150ec5c58196f6ed4a9d9d121e723be60872e8",
  "operator": "release-operator@example.org",
  "resolutions": [
    {
      "queue_entry_id": 1,
      "old_merge_commit": "<full promoted merge commit>",
      "classification": "superseded_upstream",
      "reason": "Landed through reviewed PRs and subsequently improved upstream"
    }
  ],
  "unrecorded_resolutions": [
    {
      "integration_commit": "<full unrecorded integration commit>",
      "disposition": "preserve_and_replay",
      "reason": "Reviewed operator-authored release metadata that is absent upstream"
    }
  ]
}
```

Always run the same document through a dry-run before apply:

```bash
aethyme broker integration reconcile \
  --upstream origin/main \
  --resolution-file reconciliation.json \
  --dry-run

aethyme broker integration reconcile \
  --upstream origin/main \
  --resolution-file reconciliation.json \
  --apply \
  --confirm <sha256-from-the-reviewed-dry-run>
```

The plan digest is stored in the reconciliation journal. Queue-entry operator
attestations retain the operator, reason, file path, upstream commit, and old
integration tip in queue details, reconciliation audit rows, and
`merge.externally_landed` events within the same crash-safe transaction.

Session repair is bounded by the baseline recorded at `start` or `adopt`.
Repair refuses when integration does not contain that baseline; reconcile the
upstream first instead of replaying upstream commits as session work.

### Durable PR Watches and Delivery Adapters

For continuing review after an agent has opened an open or draft PR, create a
metadata-only watch and subscribe a delivery adapter:

```bash
aethyme broker watch pr start --session 111 --repo owner/name --pr 42 \
  --events comments,reviews,checks --seconds 60 --json
aethyme broker deliveries subscribe --watch 7 \
  --adapter my-adapter --target opaque-target --policy notify --json
```

Run one bounded foreground scheduling pass with:

```bash
aethyme broker watch pr tick --limit 32 --json
```

`tick` contacts only due active watches and exits. The schema-versioned report
contains deterministic per-watch outcomes, retry times, shared rate-limit
evidence, and the next due time. Authentication failures, invalid responses,
ordinary provider errors, and rate limits receive bounded persisted backoff;
after rate-limit evidence, the remainder of the tick is deferred without more
provider calls. A host scheduler may add jitter, but must not run a watch before
its reported retry time. Aethyme never starts a background poller.

Adapters consume the provider-neutral, schema-versioned outbox with:

```bash
aethyme broker deliveries claim --adapter my-adapter \
  --worker host-worker-1 --seconds 120 --json
aethyme broker deliveries complete --id 19 \
  --worker host-worker-1 --generation 3 --outcome delivered
aethyme broker watch pr ack --id 12 --outcome addressed \
  --reason "classified and durably delivered"
```

The exact worker and generation fence completion. Expired claims can be
reclaimed with a new generation; stale workers are refused. Targets are opaque,
prompts contain allowlisted metadata rather than comment bodies, and retrieved
provider text is untrusted. `review-and-push` never grants publication authority
by itself. See [PR Review Scheduling and Delivery](../guides/pr-review-delivery.md)
for scheduler setup, adapter duties, failure recovery, and removal.

### PR Follow-Up

`aethyme broker pr check` is the first production-PR routing surface. It is
designed for local push wrappers, CI pollers, or a future Chau7/MCP bridge after
a branch has been pushed and an open PR to production exists.

Default behavior:

- target branch defaults to `production`; override with `--target <branch>`
- PR selection defaults to the open PR whose head is the current branch and base
  is the target; override with `--pr <number>`
- agent name defaults to `Push2prod`; override with `--agent <name>`
- without `--dispatch`, actionable new activity is reported and a prompt file
  may be written, but the activity is not acknowledged, so the suggested
  `--dispatch` rerun can still route it
- unchanged, all-good, or non-actionable observations may advance the local
  broker cursor in `.aethyme/broker.db`
- with `--dispatch`, the broker writes a bounded prompt under
  `.aethyme/run/pr-follow-up/`; it reuses an active/idle matching broker session
  when one exists, otherwise it starts a Codex agent command
- `--cmd <command>` overrides the default spawned command; the default command
  is a Codex exec invocation that reads the generated prompt file

Marker behavior:

- PR body contains the Unicode thumbs-up character, `:+1:`, or `:thumbsup:`:
  all good; skip comments, reviews, and checks
- PR body contains the Unicode looking-eyes character or `:eyes:`: inspect
  activity
- no marker: inspect activity

The activity fingerprint is deterministic over observed comments, reviews, and
status-check rollup. Re-running the command against unchanged PR activity will
report `new_activity: false` and will not dispatch again.

### Opt-In Review Lifecycle

The ordinary broker profile is unchanged. A repository that deliberately
delays expensive validation until review is complete may add:

```toml
[review]
schema_version = 1
enabled = true
provider = "github"
evidence_adapter = "github_approval"
required_approvals = 1
unlock_adapter = "github_label"
unlock_label = "aethyme-validation-ready"
```

The default `github_approval` evidence adapter accepts exactly one required
approval because the redacted external event contract does not claim distinct
reviewer identity. Higher thresholds are refused rather than approximated.
`github_workflow` is also available with
an explicit `workflow` value. `cloud_build_manual_trigger` is a declared
adapter boundary only: the core broker refuses to execute it and never asks
for GCP credentials. A repository-owned authenticated adapter may perform that
provider-specific action after inspecting the broker state.

Comment-only reviewers can be integrated through a repository-owned check
that converts their provider-specific result into GitHub-native, head-bound
evidence:

```toml
[review]
schema_version = 1
enabled = true
provider = "github"
evidence_adapter = "github_check_run"
required_approvals = 0
evidence_check_name = "review-gate/codex"
evidence_app_slug = "github-actions"
unlock_adapter = "github_label"
unlock_label = "aethyme-validation-ready"
```

This adapter is explicit and does not treat comments, reactions, timestamps,
or arbitrary checks as approvals. It queries at most 100 latest runs with the
exact configured name, then requires `completed`/`success`, the current full
PR head SHA, and the exact GitHub App slug. Truncated or unavailable results
are refused. The repository-owned check remains responsible for interpreting
the reviewer's native signals—for example, verifying a trusted comment author,
matching structured completion rows to the current head, and counting no
unresolved actor-owned threads. Protect that workflow from pull-request
modification, or use a dedicated GitHub App; the broad `github-actions` App
identity is safe only when the workflow producing the named check is itself a
trusted repository control.

Register only after the draft PR exists and its head is the live session HEAD:

```bash
aethyme broker review register --session 111 \
  --repo owner/name --pr 42 --json
aethyme broker submit --session 111
aethyme broker review request --session 111
```

Registration reads live GitHub evidence and binds the canonical repository,
base branch, PR number, full head SHA, and session. Successful `broker submit`
adds the exact queue entry and changes `draft_opened` to
`local_submission_verified`; no ready transition is possible before that
checkpoint. `review request` revalidates open/base/head/draft evidence, then
runs `gh pr ready` through the coordinated operation layer. A replacement
submission after `changes_requested` binds the new exact commit and restarts
review without pretending the already-ready PR became draft again.

If the owning session closes mid-lifecycle, diagnostic `review show` remains
available but mutations fail before provider access. Continue with
`review reassign --session <closed-id> --to-session <live-id> --reason <text>`
only when the live session is at the exact bound commit. Otherwise use
`review abandon --session <closed-id> --reason <text>` to retain the audit
record while freeing the pull request for a fresh registration.

Authenticated `review_changes_requested` and `review_approved` external events
advance the approval-backed lifecycle only when their repository, PR, and
commit match. An approval event does not satisfy a check-run-backed policy.
Feedback still creates an ordinary typed session advisory and never embeds
task text, review bodies, or diffs. A changes-requested event wins over an
approval for the same generation; approvals for an older SHA are stale and do
nothing.

After `review_satisfied`, unlock explicitly:

```bash
aethyme broker review show --session 111 --json
aethyme broker review unlock --session 111 --json
```

Unlock polls the configured evidence and refuses a changed base or head,
draft/closed PR, dismissed approval, unsuccessful/wrong-app check, truncated
result, or provider outage before mutation. A satisfied exact-head check can
advance `review_requested` without a webhook. The configured label or workflow
write is coordinated and journaled. Only proven success records
`validation_unlocked`; an unknown outcome activates the existing host-wide
write barrier and blind retry remains forbidden. Repeated proven transitions
are idempotent. This lifecycle authorizes publication only when the exact
promoted prefix commits the opt-in `[publication]` policy described in the
ship section; the default direct profile is unchanged.

Provisional JSON shape:

```json
{
  "target_branch": "production",
  "head_branch": "feature-branch",
  "pr": {
    "number": 42,
    "title": "Ship change",
    "url": "https://github.com/org/repo/pull/42",
    "head_branch": "feature-branch",
    "head_oid": "<commit>",
    "base_branch": "production",
    "is_draft": false,
    "review_decision": "CHANGES_REQUESTED",
    "updated_at": "2026-07-31T10:00:00Z"
  },
  "marker": "looking",
  "checked_activity": true,
  "previous_fingerprint": "old",
  "activity_fingerprint": "new",
  "new_activity": true,
  "comments": [
    {
      "kind": "comment",
      "id": "IC_kw...",
      "author": "reviewer",
      "state": null,
      "body_preview": "Please adjust this edge case.",
      "url": "https://github.com/org/repo/pull/42#issuecomment-1",
      "updated_at": "2026-07-31T10:00:00Z"
    }
  ],
  "reviews": [],
  "checks": [],
  "failing_checks": [],
  "decision": {
    "status": "needs_agent",
    "should_check_activity": true,
    "should_dispatch": true,
    "summary": "new PR activity needs Push2prod follow-up"
  },
  "prompt_path": "/repo/.aethyme/run/pr-follow-up/pr-42-178....md",
  "dispatch": {
    "status": "not_requested",
    "session_id": null,
    "prompt_path": "/repo/.aethyme/run/pr-follow-up/pr-42-178....md",
    "command": null,
    "message": "dispatch not requested; prompt is ready on disk"
  },
  "next_commands": [
    "aethyme broker pr check --target 'production' --pr 42 --dispatch"
  ]
}
```

Current scope limits:

- the broker reads GitHub through `gh`; callers are responsible for
  authentication and network access
- the broker does not reply to comments, resolve review threads, push commits,
  or mutate GitHub PR state; the dispatched agent does that work
- Git has no native post-push hook, so "after successful push" automation should
  call this command from a push wrapper, CI job, webhook worker, or Chau7/MCP
  controller rather than from `pre-push`

## Core Commands

### Indexing
- `aethyme index PATH --name NAME --languages python,typescript --use-fallback`
- `aethyme stats`

### Local Repo Intake
- `aethyme repo ingest /path/to/repo`
- `aethyme repo inspect /path/to/repo --json-output`
- `aethyme repo clear-cache /path/to/repo`
- `aethyme repo warm /path/to/repo`
- `aethyme repo compile-skills /path/to/repo`
- `aethyme repo init-onboarding-overrides /path/to/repo`
- `aethyme repo validate-onboarding-overrides /path/to/repo`
- `aethyme repo init-agents-overrides /path/to/repo`
- `aethyme repo validate-agents-overrides /path/to/repo`
- `aethyme repo experience-telemetry /path/to/repo`
- `aethyme repo experience-telemetry /path/to/repo --check`
- `aethyme repo experience-status /path/to/repo`
- `aethyme repo commit-message-template --type fix --scope watchlist`
- `aethyme repo lint-commit-message .git/COMMIT_EDITMSG`
- `aethyme repo lint-commit-message --message "docs(cli): clarify examples"`
- `aethyme repo deploy-skills /path/to/repo --force`
- `aethyme repo engine-info --json-output`
- `aethyme repo engine-info --check`

`repo compile-skills` generates repo-specific skills, currently
`repo-onboarding`, into `.aethyme/generated/` plus per-product skill paths.
It also records summon policy and generation telemetry inside the generated
artifact. Maintainers can override selected sections with
`.aethyme/overrides/onboarding.json`.
It also generates a deterministic `repo-act` starter artifact and skill for
debugging and validation planning.

Example override:

```json
{
  "commands": [
    {
      "kind": "test",
      "command": "./scripts/test-fast.sh",
      "source": "manual-override",
      "confidence": "high"
    }
  ],
  "notes": [
    "Use sandbox credentials from 1Password.",
    "Do not edit src/gen directly; run pnpm codegen."
  ]
}
```

`notes[]` are rendered into the visible `repo-onboarding` skill under
`Maintainer Notes`; humans contribute by editing the override file and
regenerating onboarding, not by editing generated skill files directly.

`repo init-onboarding-overrides` writes a starter override file.
`repo validate-onboarding-overrides` checks that the override file is valid JSON
and that key fields use the expected shapes. Unknown top-level keys and unknown
keys inside `repo` or `summon` fail with the exact JSON path and the allowed
keys; misspellings are never silently ignored.

`repo init-agents-overrides` writes a starter `.aethyme/overrides/agents.json`
file. Use it for repo-specific root instruction customization such as:
- repo summary
- hard constraints
- validation rules
- commit hygiene notes
- summon policy notes
- migrated maintainer markdown

`repo validate-agents-overrides` checks that the agents override file is valid
JSON and that those fields use the expected shapes. Its schema is closed:
unknown fields fail with an exact `$.field` diagnostic and the allowed keys.

Broker-enrolled repositories use progressive disclosure by default. Generated
`AGENTS.md` and `CLAUDE.md` remain byte-identical and retain mandatory Explore,
session lifecycle, refusal, advisory, and publication-authorization rules, but
are contract-tested to stay at or below 12,000 bytes. Detailed lease, gate,
resource, cleanup, operation, ship, and recovery procedures are generated at
both `.codex/skills/aethyme/references/broker.md` and
`.claude/skills/aethyme/references/broker.md`. Agents load that local reference
when those workflows apply; no network lookup is required. Generated policy is
deterministic and contains no wall-clock timestamp.

Direct root-policy edits remain protected by the repository upgrade planner.
Content that differs from the recorded generated receipt is classified as
customized and requires an explicit preserve, merge, or replace resolution;
neither deploy nor upgrade silently treats it as a known generated version.

`repo deploy-skills` is now a compatibility path that deploys only the static
runtime navigation skill. For real repositories, prefer
`aethyme enhance deploy --repo /path/to/repo`.

`repo commit-message-template` prints the typed commit message skeleton Aethyme
expects for durable commit hygiene. `repo lint-commit-message` validates a real
message against that contract and emits structured JSON suitable for future
memory extraction.

Commit hygiene contract:
- subject: `type(scope): short summary` or `type: short summary`
- allowed types: `fix`, `feat`, `refactor`, `perf`, `test`, `docs`, `build`, `chore`, `revert`
- substantive types `fix`, `feat`, `refactor`, and `perf` require structured
  body sections: `Problem`, `Decision`, `Rationale`, `Validation`
- non-substantive types `test`, `docs`, `build`, `chore`, and `revert` may use
  a subject-only message; their structured bodies are optional and are still parsed
- section content may begin on the header line (`Problem: text`) or on the next
  line after a standalone header (`Problem:` followed by `text`)
- optional sections: `Alternatives considered`, `Risks`, `Follow-up`, `Memory`

Substantive example using both section forms:

```text
fix(watchlist): mark only viewed revision as seen

Problem: Viewing a diff marked every revision as seen.

Decision:
Use the viewed revision id for seen-marking.

Rationale: Seen state is revision-scoped.

Validation:
- Added regression coverage.
- Ran watchlist tests.

Memory:
Watchlist seen-marking must remain revision-scoped.
```

Non-substantive subject-only example:

```text
docs(cli): clarify commit hygiene examples
```

### Local Discoverability
- `aethyme deploy --repo /path/to/repo`
- `aethyme deploy plan --repo /path/to/repo --diff`
- `aethyme deploy execute --repo /path/to/repo --confirm <plan-sha256>`
- `aethyme deploy verify --repo /path/to/repo`
- `aethyme enhance deploy --repo /path/to/repo`
- `aethyme enhance verify --repo /path/to/repo`
- `aethyme query symbol /path/to/repo main`
- `aethyme query deps /path/to/repo src/main.py`
- `aethyme query impact /path/to/repo src/main.py`

`deploy plan` is the read-only first-enrollment path. It derives the proposed
tree from the exact fetched remote default SHA in a disposable checkout and
binds generated hashes and modes, local/integration state, dirty overlap, live
broker state, hook ownership, activation state, and preservation refs into a
SHA-256. `--diff` renders the content inventory locally; `--json` is the stable
automation form.

`deploy execute` requires that full digest. It creates preservation refs before
mutation, applies only the reviewed outputs in an isolated leased session,
submits and publishes the promoted SHA, verifies the remote, and synchronizes
local main only by a verified clean fast-forward. Its Git-common-dir journal
makes the same command resumable across interruption. Remote movement,
ambiguous history, foreign hook ownership, overlapping dirty policy, or live
coordination work causes a refusal and requires a new plan or explicit cleanup.

The bare `deploy` command remains the offline/manual preparation path. It runs
broker scaffold, gate drafting, embedded agent-policy deployment, verification,
and certification without committing or publishing. `deploy verify` performs
the verification and certification checks without writing. Repositories should
retain this command as a required CI check.

For staged team adoption, `deploy bridge` appends an inert managed block to
`AGENTS.md` and `CLAUDE.md`; review and commit those two files. Individual
developers then run `deploy --local-only`, which activates the complete policy
behind `.aethyme/local/enabled` and excludes its files through local Git
metadata rather than tracked `.gitignore`. `deploy verify --local-only` is
read-only. Inactive clones perform only the bridge's marker existence check and
do not probe for or invoke the binary.

`enhance deploy` is the lower-level discoverability operation used by the
canonical command. It writes:
- fully generated `AGENTS.md`
- `CLAUDE.md`
- `.claude/skills/aethyme/SKILL.md`
- `.codex/skills/aethyme/SKILL.md`
- `.claude/skills/aethyme/references/*.md`
- `.codex/skills/aethyme/references/*.md`
- `.claude/hooks/aethyme-load-context.sh`
- `.aethyme/generated/onboarding.json`
- `.aethyme/generated/act-starter.json`
- `.claude/skills/repo-onboarding/SKILL.md`
- `.codex/skills/repo-onboarding/SKILL.md`
- `.claude/skills/repo-act/SKILL.md`
- `.codex/skills/repo-act/SKILL.md`

`AGENTS.md` and `CLAUDE.md` are now generated artifacts owned by Aethyme.
Customize them through `.aethyme/overrides/agents.json`, not by editing the
root files directly. The generated root instructions include:
- native Explore guidance
- repo-onboarding and repo-act routing
- experience status path
- primary fast test when detected
- primary app entrypoint when detected
- commit hygiene policy and commands
- broker coordination when configured: verified local submission is the
  default, explicitly authorized agents retain the full Git command surface,
  and every operation affecting shared refs or remote state is routed through
  the broker

Legacy block-managed `AGENTS.md` files are migration-only now. On deploy,
Aethyme extracts maintainer-authored legacy content into
`.aethyme/overrides/agents.json` and then rewrites the root file as a fully
generated artifact.

`onboarding.json` is the canonical artifact. It includes:
- repo identity
- inferred commands, areas, entrypoints, caution zones
- summon rules for when the onboarding skill should be loaded
- freshness metadata
- generation telemetry and override status

`act-starter.json` is the deterministic execution companion artifact. It includes:
- debugging and validation starter checklists
- likely fast test/lint/build commands
- likely entrypoints and caution zones

`enhance verify` also prints a compact summary: recommended skill/mode,
onboarding counts, override presence, override freshness, and Act starter
readiness. Direct edits to `AGENTS.md` or `CLAUDE.md` are now verification
failures; use `.aethyme/overrides/agents.json` instead.

After a successful `deploy`, commit `.gitignore`, broker configuration and
gates, overrides, the two canonical onboarding JSON artifacts, `AGENTS.md`,
`CLAUDE.md`, and the `.codex`/`.claude` policy trees. Broker databases, logs,
reports, run state, worktrees, conflict handoffs, experience telemetry, and
experience status projections remain ignored machine-local state. See
[`../guides/repository-deployment.md`](../guides/repository-deployment.md).

Stable experience-layer telemetry is written to:
- `.aethyme/generated/experience-telemetry.jsonl`

Generated experience status artifacts are written to:
- `.aethyme/generated/experience-status.json`
- `.aethyme/generated/experience-status.md`

Inspect it with:
- `aethyme repo experience-telemetry /path/to/repo`
- `aethyme repo experience-telemetry /path/to/repo --json-output`
- `aethyme repo experience-telemetry /path/to/repo --check`
- `aethyme repo experience-status /path/to/repo`
- `aethyme repo experience-status /path/to/repo --json-output`

The report now derives simple experience-layer KPIs, for example:
- enhancement installed but no wrapper usage recorded yet
- invalid onboarding override present
- onboarding exists but no fast test command detected
- onboarding overrides changed after generated artifacts and need regeneration

`--check` exits nonzero when attention signals are present, so it can be used in
CI or local verification gates without parsing the full report.

`repo experience-status` writes a compact operator artifact with:
- enhancement installed/verified state
- onboarding/Act presence
- override freshness
- KPI signals and suggestions
- recommended next command

It also emits concrete suggestions tied to those signals, for example:
- load onboarding and use the Aethyme wrapper on the next broad task
- fix or reinitialize an invalid override
- add a fast test command through onboarding overrides

This ledger records deterministic lifecycle events only, such as:
- `enhance.deploy`
- `enhance.verify`
- `repo.compile-skills`
- `repo.init-onboarding-overrides`
- `repo.validate-onboarding-overrides`

Wrapper-level signals are also recorded when Aethyme-provided entry points are
actually invoked:
- `wrapper.invocation` with `wrapper_name=aethyme-explore`
- `wrapper.invocation` with `wrapper_name=aethyme-sessionstart-hook`

It does not yet claim actual agent adoption or downstream answer quality.

### Authoritative Graph Lifecycle

The installed `aethyme` router owns the complete version-safe lifecycle; a
separate graph-indexer executable is not required:

```bash
aethyme graph status --repo . [--json]
aethyme graph materialize --repo . [--json]
aethyme graph refresh plan --repo . [--json | --diff]
aethyme graph refresh execute --repo . --confirm <plan-sha256> [--json]
aethyme graph refresh recover --repo . --plan <plan-sha256>
```

`status` inspects typed policy plus the committed graph manifest without
cloning or indexing; deliberate disabled authority is healthy and requires no
action. `plan` alone regenerates from exact committed `HEAD` in a disposable
clone. Neither reads incidental source bytes from the active worktree. The
stable JSON plan reports the canonical repository, source commit
and tree, graph policy and pin, linked component versions, hash-only fragment
write set, derived-store action, dirty overlap, live sessions, relevant leases,
compatibility, blockers, and a SHA-256 binding all of those preconditions.
Neither JSON nor the hash-only `--diff` surface contains source contents or
absolute repository paths.

Graph support is disabled by default. Canonical setup opts in with
`aethyme deploy --repo . --with-graph`; use
`--graph-repository owner/name` only when no canonical `origin` can be
resolved. Enrollment writes policy plus the exact engine pin but defers all
generation until those files are reviewed and committed. `--with-graph` is
refused for `--local-only` because untracked policy cannot authorize shared
committed fragments.

All lifecycle JSON reports include content-free phase timings, observable byte
counts, graph entity counts when available, and process peak RSS. Confirmed
refresh supports `--json` so benchmark and diagnostic tooling can capture the
actual revalidated execution rather than only its preliminary plan. Timing and
memory evidence are excluded from the deterministic plan digest.

`materialize` validates committed policy, pin, manifest, and fragment bytes
against exact `HEAD`, then atomically builds only the ignored worktree-local
redb store. It never clones, parses source, or regenerates fragments. It does
not change tracked files, accepts no hidden network input, reports elapsed
milliseconds plus clone/index operation counts in stable JSON, and is a no-op
when the store is current.

If the local store is missing, materialization can install a verified immutable
host-cache artifact keyed by source tree, fragment-manifest digest, engine and
protocol version, and storage schema. JSON `cache.status` is `not_needed`,
`unavailable`, `hit`, `miss_stored`, or `miss_unstored`; it also reports the
key digest and artifact bytes without exposing a host path. A hit is copied to
private staging, schema-validated, rebound to the receiving worktree, and
atomically published. Cached redb files are never opened writable or shared
between worktrees. Set `AETHYME_HOST_CACHE_DIR` only to relocate the derived
cache or isolate a benchmark; deleting it is safe because committed fragments
remain authoritative.

`execute` revalidates the complete plan under an exclusive repository lock,
requires the full digest, writes a private rollback/recovery journal, applies
fragment files through sibling temporary files, verifies every resulting hash,
and only then builds and atomically publishes `.aethyme/graph_store.redb` from
the reviewed committed inputs. A crash never authorizes an implicit retry;
`recover` completes only the exact digest-confirmed journal. Exact generated
outputs already present but not committed are recognized and routed to review
and commit. Disjoint dirty files are preserved and explicitly reported as not
being plan inputs; overlapping graph changes and live broker sessions block.

The repository engine-version pin is never rewritten by refresh. If the pin
does not match the installed release unit, install the signed compatible
release or migrate the repository contract through the reviewed upgrade flow.
Ordinary Explore performs no background download or transparent refresh.
When its optional local store is unavailable, Explore exits successfully with
schema-valid degraded answer JSON, empty evidence, false answer/navigation
safety, path-redacted observability, and an explicit manual or materialization
next action.
The complete operational workflow, including post-commit restamping and old
pin handling, is in [Version-safe graph refresh](../guides/graph-refresh.md).
Reproducible release-mode measurement and interpretation are documented in
[Graph performance evidence](../guides/graph-performance.md).
The lower-level `aethyme-engine-cli index` surface remains available for
engine diagnostics, but it does not provide the plan, confirmation, dirty-path,
session, lease, or recovery contract required for authoritative fragments.

### High-Level Intent Surface

> **Note (2026-08-01):** every command is served by the native Rust binary. The Python CLI that once carried a targeted recovery error for `explore` is itself deleted; `python -m src.cli` now fails with `No module named src`.

- `aethyme explore --repo /path/to/repo --request "Find public functions with no outside callers" --format answer-json`
- `aethyme intents --request "Find public functions with no outside callers" --format compact-json`
- `aethyme explore --repo /path/to/repo --intent behavior_localization_query --request "Find the files responsible for this behavior" --format answer-json --show-observability`
- `aethyme explore --repo /path/to/repo --intent behavior_localization_query --request "Find the files responsible for this behavior" --format answer-json --show-observability --detail full`
- `aethyme explore --repo /path/to/repo --intent usage_boundary_query --request "Find public functions with no outside callers" --params '{"scope":"src/pkg","symbol_kind":"public_top_level_function","boundary":{"type":"outside_directory","path":"src/pkg"},"search_roots":["src","tests"],"budget_ms":10000,"max_evidence_per_symbol":5}' --format answer-json --show-observability`

`intents` returns the finite mode/intent catalog. The public product model is
`explore / act / learn`; `explore` is the implemented primary mode today and
ships the default `task_localization_query` intent plus specialized intents
such as `behavior_localization_query` and `usage_boundary_query`. `act` and
`learn` are product-direction modes, not equivalent top-level CLI groups yet.

`explore --request ...` without `--intent` runs the default
`task_localization_query` intent. It composes one bounded `task-localize` graph
call, bounded deterministic symbol search, source-text ranking, source
call-site expansion, filename fallback, and compact `task-expand` output into:
- `answer[]`: ranked graph/symbol/source-backed candidate files, symbols, areas, call-site files, and next-step targets
- `navigation_hints[]`: low-confidence investigation hints, including filename-only fallback candidates and suggested searches
- `excluded[]`: out-of-scope areas or candidates
- `ambiguous[]`: low-confidence or missing-anchor cases
- `subsystems[]`: ranked subsystem lanes for ambiguous Surface/Flow tasks, including role, confidence, concrete `token_subsystems`, top verification targets, paths, signals, and missing-coverage warnings; broad auth/token requests use this to separate ingress/proxy, backend validation, and provider/OIDC/audit-style token systems before trusting a flat file ranking
- `output_chars_estimate` / `truncated`: command-output budget metadata for agent loops
- `output_adapters.task_localization_json`: compact candidate file/symbol lists and expansion commands, emitted only with `--detail full`
- `confidence`: answer-only, excluded-only, and analyzed confidence summaries
- `safe_to_use_as_answer` / `trust_policy`: whether `answer[]` is authoritative enough to guide an answer, or only safe as navigation
- `observability`: with `--show-observability`, compact graph-store freshness, Surface/Flow coverage, missing expected surfaces, ranking explainability, answer-safety mode, and readiness fields. Freshness alone is not enough: agents should require the graph to be fresh, complete enough for the request, and explainable before treating `answer[]` as answer-safe. Use `--detail full --show-observability` only when debugging the full observability envelope.

For task/behavior localization, Explore observability includes:
- `graph_freshness`: redb backend status, `fresh`, `stale`, fragment/store timestamps, and path-free artifact role labels (`source_of_truth=graph_fragments`, `derived_query_artifact=redb_graph_store`)
- `surface_flow_graph.coverage`: per-surface coverage for backend, edge/proxy, routes, middleware, webhooks, CLIs, jobs/queues, credential flows, and live behavior tests
- `indexed_languages` / `indexed_frameworks`: language/framework signals inferred from indexed graph fragments, not from source files alone
- `surface_flow_graph.missing_expected_surfaces`: source-present surfaces that graph fragments did not fully index
- `ranking_explainability`: `degraded_ranking_reasons`, `top_signals_used`, `top_signals_absent`, and whether subsystem ambiguity was detected
- `answer_safety`: evidence-only safety, observability-adjusted safety, navigation-only mode, trust policy, and reason
- `readiness`: booleans for `fresh_enough`, `complete_enough`, `surface_flow_complete`, `explainable`, `answer_safe_after_observability`, and `navigation_only_after_observability`

Compact agent-mode observability is the default for `--show-observability`.
It caps `answer[]`, `navigation_hints[]`, subsystem targets/signals, ranking
signals, evidence arrays, and Surface/Flow path hints so an initial Explore
call stays in the 12k-20k character budget. Indexed language/framework detail
and full path-hint coverage remain available through
`--detail full --show-observability`.

Default `task_localization_query` responsiveness behavior:
- `graph_query_timeout_ms`: default `1000`
- `symbol_query_timeout_ms`: default `1000`
- `skip_symbols_after_graph_timeout`: default `false`; if graph localization times out, Aethyme still attempts bounded symbol and source-text recovery unless the caller opts out.
- The command returns degraded partial output with `degraded_reasons` instead of blocking indefinitely.
- If source-text/call-site evidence is strong enough, degraded output may still set `safe_to_use_as_answer=true`; inspect `observability.degradation_guidance`, `answer[].evidence.line_refs`, and `evidence.callsite_expansions` before trusting it.
- Filename-only evidence is low confidence and cannot set `safe_to_use_as_answer=true`.
- For very large repos where first response speed matters more than graph coverage, callers can lower `graph_query_timeout_ms` to `500`.

`explore --intent behavior_localization_query` is the preferred generic path for
debugging, feature localization, and "which files implement this behavior?"
questions. It uses the same answer schema as the default Explore path but gives
source-text ranking and call-site expansion a larger budget. It is still
repository-agnostic and does not inject benchmark-specific candidates.

Use `intents` or explicit `--intent` when the caller/LLM can select a more
precise deterministic analyzer from the catalog. Aethyme still does not perform
rich free-form routing; the default path is the general repository localization
intent, not a hidden task-specific guess.

`explore --intent usage_boundary_query` is the preferred task-ready entry point
for public-symbol usage boundary questions, including dead-code checks. The LLM
chooses the intent and supplies structured params; Aethyme performs deterministic
analysis and returns:
- `answer[]`: primary task result
- `excluded[]`: candidates rejected by evidence
- `output_adapters.dead_code_eval_json`: compatibility shape for the dead-code eval
- `confidence`: answer-only, excluded-only, and analyzed confidence summaries
- `observability`: the same enterprise envelope used by the default Explore path, plus `usage_boundary_analyzer` graph/fact counts, confidence summary, and analyzer degraded reasons. Usage-boundary remains a hybrid contract: redb discovers seeds/candidate files, while source text supplies caller/docs/config evidence.

The current `usage_boundary_query` implementation uses the scope-first
`analyze-usage-boundary` engine path for PHP public methods/functions. That path
opens `.aethyme/graph_store.redb` read-only to discover public symbols and
candidate files, then scans source/docs/config text for evidence. It does not
build `RepositoryMap` or mutate the store; use `aethyme graph status --repo
<repo>` and the reviewed `graph refresh plan`/`execute` flow if the redb
artifact is missing. For non-PHP scopes, or when
`degraded_reasons` includes language/support gaps, use the graph-backed
`analyze dead-code` / `facts function-usage` commands as the fallback.

Phase 5 decision: usage-boundary is intentionally hybrid V2, not fully
redb-native. redb owns seed discovery; query-time source/docs/config scanning
owns evidence strings so caller lines and docs/config references reflect the
current checkout. A fully redb-native analyzer would need persisted evidence
rows plus freshness/invalidation rules before replacing this source-text pass.

Optional params:
- `budget_ms`: time budget for the scope-first analyzer, default `10000`
- `max_evidence_per_symbol`: maximum evidence strings retained per symbol, default `5`

### Graph Navigation
- `aethyme graph node /path/to/repo <target> --json-output`
- `aethyme graph children /path/to/repo <target> --json-output`
- `aethyme graph parents /path/to/repo <target> --json-output`
- `aethyme graph callers /path/to/repo <target> --json-output`
- `aethyme graph callees /path/to/repo <target> --json-output`
- `aethyme graph docs /path/to/repo <target> --json-output`
- `aethyme graph configs /path/to/repo <target> --json-output`
- `aethyme graph overview /path/to/repo --json-output`

`node`, relation, and expansion graph commands read
`.aethyme/graph_store.redb` through read-only engine APIs. `graph overview`
still uses the in-memory graph overview path until it receives its own redb
adapter.

### Derived Facts
- `aethyme facts public-functions --repo /path/to/repo --scope src/pkg --json-output`
- `aethyme facts function-usage --repo /path/to/repo --target my_function --boundary src/pkg --json-output`
- Add `--roots src,tests` to `function-usage` when the repository is large and the relevant search roots are known.

### Deterministic Analyzers
- Prefer `aethyme explore --intent usage_boundary_query` for task-ready boundary usage answers.
- `aethyme analyze dead-code --repo /path/to/repo --scope src/pkg --boundary outside-directory --format eval-json --show-observability`
- `aethyme analyze dead-code --repo /path/to/repo --scope src/pkg --format full-json`
- Add `--include-methods` when class/object methods are in scope.
- Add `--roots src,tests` to narrow caller evidence collection on large repositories.
- `--format eval-json` emits `unused_functions[]` items with `function_name`,
  `defined_in`, `status`, `external_callers`, `internal_callers`, `evidence`,
  `confidence`, and `reason`, plus `excluded_functions[]`.
- `--show-observability` adds command name, repository path, index freshness,
  graph/fact counts, output size, confidence summary, and degraded reasons.
- `--json-output` remains supported as a compatibility alias for
  `--format full-json`.

### Local Task Packs
- `aethyme task pack --repo /path/to/repo --task "Explain this repo" --json-output`
- `aethyme task explain --repo /path/to/repo`
- `aethyme task anchors --repo /path/to/repo --task "..." --json-output`
- `aethyme task scope --repo /path/to/repo --task "..." --json-output`
- `aethyme task next --repo /path/to/repo --task "..." --json-output`
- `aethyme task expand --repo /path/to/repo --node <target> --json-output`
- `aethyme task context --repo /path/to/repo --task "..." --json-output`

`task anchors`, `task scope`, `task next`, `task expand`, `task pack`,
`task explain`, and `task context` read the redb graph store. Source text is
still read from the filesystem when context packs need snippets/content, but
candidate selection and graph navigation come from `.aethyme/graph_store.redb`.

### Local Evaluation
- `aethyme eval explain-repo --repo /path/to/repo --json-output`
- `aethyme eval explain-repo --repo /path/to/repo --control-cmd "<cmd>" --explore-cmd "<cmd>" --leverage-cmd "<cmd>"`
- `aethyme eval navigation-ctf --repo /path/to/repo --json-output`
- Example Codex wrapper command: `packages/aethyme-eval/.venv/bin/python packages/aethyme-eval/scripts/run_codex_eval.py` (eval tooling is Python and lives in the separate `aethyme-eval` package; `packages/aethyme` carries none)
- Example regression gate command: `packages/aethyme-eval/.venv/bin/python packages/aethyme-eval/scripts/check_regression_gate.py --suite /path/to/suite.json`

Current behavior:
- with no commands, this builds the control artifacts and comparison report only
- with `--control-cmd`, `--explore-cmd`, and `--leverage-cmd`, it executes real runs through the evaluation runner contract
- `--baseline-cmd` and `--aethyme-cmd` remain accepted as legacy aliases for compatibility
- external runners receive the prompt, navigation context, output schema, and Aethyme tool paths through `AETHYME_EVAL_*` env vars
- the bundled Codex wrapper requires `AETHYME_EVAL_ARM=control|aethyme`, runs `codex exec --ignore-user-config --json`, preserves `events.jsonl` / `stderr.log` / `last-message.json` / `leakage.json`, reports wall time, total input/output tokens, cached input tokens, uncached input tokens, uncached-plus-output budget tokens, command-output chars, event-log chars, stderr chars, fixture metadata, and output fingerprints, and fails the run if generated Aethyme artifacts leak into selected files, snippets, command output, or the final answer
- the strict regression gate rejects Aethyme self-evals, incomplete required fixture suites, generated-artifact leakage, missing repeat-output determinism, unbounded command output, uncached-plus-output token-budget regressions, worse reviewer quality, and hidden Surface/Flow coverage gaps; total token estimate remains context-pressure telemetry
- every run writes a local markdown report under `packages/aethyme/docs/reports/evals/`
- the repository tracks only a curated subset of eval reports there; the rest are generated local artifacts
- JSON output includes the generated `report_path`
- evaluation JSON now includes `output_schema`, `scoring_rubric`, and `reference_output`

## Local Runtime Notes

- the router executes the engine in-process, auto-starting the engine daemon when needed; no interpreter is involved
- local artifacts are cached by repository snapshot under `AETHYME_CACHE_DIR` or `/tmp/aethyme-cache`
- Git repositories use commit plus dirty-state metadata for cache keys
- `repo clear-cache` clears the current snapshot cache
- the Aethyme-assisted evaluation prompt uses a compact rendered pack rather than the full raw JSON payload

### Graph Queries
- `aethyme search TERM --limit 20 --type hybrid`
- `aethyme ego SYMBOL --depth 2`
- `aethyme impact SYMBOL --max-depth 10`

### Scorecard
- `aethyme ai-ready --repo PATH --format md`

### Autofix
- `aethyme autofix PATH --dry-run`
- `aethyme autofix PATH --apply`
- `aethyme autofix PATH --pr`

## Rule

CLI commands should keep using the same indexing and graph contracts as the API.
