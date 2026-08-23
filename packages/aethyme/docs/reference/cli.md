# CLI Reference

Last Updated: 2026-08-23

## Install

```bash
cargo install --path packages/aethyme/rust/crates/aethyme-cli
cargo install --path packages/aethyme/rust/crates/aethyme-engine
```

Publishing a release does not update installed Cargo binaries. Users must
reinstall both packages from the desired release checkout (or install the
published version explicitly) and verify `aethyme --version`. Aethyme has no
self-update daemon or silent background upgrade path.

`aethyme` is a single Rust binary; no interpreter, virtualenv, or pip
step is involved. **`python -m src.cli` no longer exists** — the Python
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

- `aethyme init`
- `aethyme certify`
- `aethyme broker status`
- `aethyme broker start`
- `aethyme broker adopt`
- `aethyme broker exec`
- `aethyme broker git`
- `aethyme broker gh`
- `aethyme broker operations`
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

## Broker Commands

The broker is the stable product front door for multi-agent coordination:

For task-oriented examples that connect session reuse, gate cache policy,
lease planning, and durable finish handoffs, see the
[broker follow-up workflows guide](../guides/broker-workflows.md).

- `aethyme init`
- `aethyme certify`
- `aethyme broker status [--json]`
- `aethyme broker start --task "..." [--json]`
- `aethyme broker adopt [<path>] --task "..." [--reuse [--sync-integration]] [--json]`
- `aethyme broker exec --session <id> -- <command> [--json]`
- `aethyme broker git --session <id> [--repo <owner/name>] [--scope <scope>] [--effect <read|write|destructive>] [--reason <text>] [--destructive] -- <git-args>`
- `aethyme broker gh --session <id> --repo <owner/name> [--scope <scope>] [--effect <read|write|destructive>] [--reason <text>] [--destructive] -- <gh-args>`
- `aethyme broker operations [--json]`
- `aethyme broker operations reconcile --operation <id> --outcome <succeeded|failed> --reason <text> [--json]`
- `aethyme broker gates affected --session <id> [--why] [--json]`
- `aethyme broker gates semantic --session <id> [--json]`
- `aethyme broker gates run --session <id> [--no-cache] [--json]`
- `aethyme broker gates run --all [--no-cache] [--json]`
- `aethyme broker hooks install [--json]`
- `aethyme broker hooks uninstall [--json]`
- `aethyme broker hooks status [--json]`
- `aethyme broker leases plan <paths...> [--session <id>] [--json]`
- `aethyme broker submit --session <id> [--no-cache] [--json]`
- `aethyme broker repair --session <id> [--json]`
- `aethyme broker finish --session <id> [--json]`
- `aethyme broker handoff (--session <id> | --worktree <path>) [--json]`
- `aethyme broker report capture --kind <bug|improvement> --title <text> [--session <id>] [--include-task] [--stdout | --output <filename>] [--json]`
- `aethyme broker report list [--json]`
- `aethyme broker report show <filename> [--json]`
- `aethyme broker report render <filename> --form <form.yml> [--output <name>.issue.md] [--json]`
- `aethyme broker report file <path> --repo <owner/name> --confirm <sha256> [--json]`
- `aethyme broker ship plan --entry <id> [--json]`
- `aethyme broker ship execute --entry <id> --confirm <full-integration-sha> [--sync-main] [--json]`
- `aethyme broker integration status [--json]`
- `aethyme broker integration reconcile --upstream <ref> [--resolution-file <path>] [--dry-run|--apply] [--json]`
- `aethyme broker quick-test [--with-gate] [--json]`
- `aethyme broker verify-loop [--json]`
- `aethyme broker pr check [--target <branch>] [--pr <number>] [--agent <name>] [--dispatch] [--cmd <command>] [--json]`

`quick-test` is the disposable install smoke. `verify-loop` is the stronger
operator E2E: it reports the integration commit tested and flags movement during
the run, so callers know whether the result proves the current integration tip.

`broker adopt --reuse --sync-integration` starts a follow-up from the current
integration tip. It requires a clean session worktree, permits only a
fast-forward, and synchronizes before recording the follow-up diff baseline;
dirty or diverged worktrees are left unchanged.

`broker submit` builds a normalized commit-provenance plan before gate
selection. It replays only pending `session_owned` single-parent patches onto
the exact integration tip, in order. Patch-equivalent history already present
under another SHA is classified as
`already_integrated_by_stable_patch_identity` and is not replayed. Missing
baselines, ambiguous ownership or patch identity, and pending owned merge
commits are refused rather than guessed.

With `--json`, `submission_plan` exposes the full recorded baseline, session
HEAD, integration HEAD, ordered commits, their parents, ownership,
integration state, stable patch ID, matching integration commits, safety flag,
and warnings. On rejection, `conflict_details` supplements the compatible
`conflicts` path list with the full originating commit, ownership, known
integration-side commits, remediation text, and ordered commands. A blocking
session is reported only when its current active lease overlaps a surviving
replay conflict.

Gate-run and submit outcomes identify the exact Git tree each result proves.
Human-readable output abbreviates the tree hash to 12 characters; JSON retains
the full hash in `tree_hash` for both executed and cached results.
Pass `--no-cache` to either gate-run form or submit to require fresh gate
execution. Bypass skips cache lookup only: the fresh result is stored normally
and is available to a subsequent run using the default cache policy.

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

`broker hooks install` installs shared pre-commit and post-commit shims. The
pre-commit hook runs matching cost-1 gates against the staged change and stays
silent when they pass. If a gate fails, the hook replays its complete standard
output and error, prints the broker diagnosis, returns the gate's non-zero exit
code, and blocks the commit. The one-shot Git escape hatch remains
`git commit --no-verify`.

`broker leases plan` is a read-only preflight for files or trailing-slash
directory claims. It reports exact and directory overlaps with each active
lease's owning session, implicit or explicit kind, and expiry. Supplying
`--session` separates leases already owned by that session from foreign
conflicts; without it, every overlap is a potential conflict. Planning neither
claims nor refreshes leases and does not append broker events or command
telemetry. Paths are sorted deterministically and must be unambiguous,
repository-relative spellings without `.` or `..` components.

`broker finish` returns a structured handoff covering the latest queue and
submitted/promoted/published delivery state, pending work, every recorded
active/released/expired lease, the latest executed or cache-resolved gate with
its full tree hash and event time, cleanup safety, and one recommended next
action. A successful close persists the same operational fields in a redacted
`session.finished` event atomically with `session.cleaned`, so the handoff
survives lease cleanup and session closure. Refused and already-closed finishes
do not emit a misleading or duplicate completion event.

Retrieve the newest persisted handoff without changing broker state:

```bash
aethyme broker handoff --session 110
aethyme broker handoff --worktree .aethyme/worktrees/my-task --json
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
inspect the promoted queue entry, exact integration SHA, remote freshness,
proposed non-force push, and local-main safety without mutating refs or remote
state. Publish only with the plan's full integration SHA:

```bash
aethyme broker ship plan --entry 42
aethyme broker ship execute --entry 42 \
  --confirm 0123456789abcdef0123456789abcdef01234567
```

Execution fetches and revalidates the planned remote base, requires a
fast-forward, pushes that exact SHA, and verifies the remote default ref. It
leaves the primary checkout unchanged unless `--sync-main` is present; that
option additionally requires a clean, unchanged, fast-forwardable local
default branch. `broker integration status` reports whether the integration tip
is promoted, published, or locally synchronized and routes its next action
through this ship lane.

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
command may apply only part of its requested change before failing. V1
deliberately serializes all writes for one repository.

`integration reconcile` is the recovery path when promoted work lands outside
the broker, including squash merges. It never fetches: first update the remote
tracking ref explicitly, then run the default dry-run. Exact ancestry, stable
patch IDs, and path-tree equivalence classify externally landed work; remaining
promotions are replayed in queue order on the named upstream. `--apply` moves
the integration ref and queue state together. Ambiguous equivalence or a replay
conflict blocks without changing either one. A durable two-phase intent makes
the update crash-safe: the next broker open either completes the queue/audit
transaction when the ref moved or cancels the intent when it did not.

When automatic evidence correctly fails closed because landed work was later
modified upstream, an operator can attest only the affected entries with a
versioned resolution file. The file is single-use by construction: it binds the
named ref's exact fetched commit, the old integration tip, each queue ID, and
each original promoted merge commit. Only `superseded_upstream` is accepted;
unknown fields, duplicate entries, stale commits, empty reasons, and redundant
overrides of automatic matches are rejected before planning or mutation.

```json
{
  "schema_version": 1,
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
  --apply
```

The operator, reason, file path, upstream commit, and old integration tip are
stored in the queue details, reconciliation audit row, and
`merge.externally_landed` event within the same crash-safe transaction.

Session repair is bounded by the baseline recorded at `start` or `adopt`.
Repair refuses when integration does not contain that baseline; reconcile the
upstream first instead of replaying upstream commits as session work.

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
and that key fields use the expected shapes.

`repo init-agents-overrides` writes a starter `.aethyme/overrides/agents.json`
file. Use it for repo-specific root instruction customization such as:
- repo summary
- hard constraints
- validation rules
- commit hygiene notes
- summon policy notes
- migrated maintainer markdown

`repo validate-agents-overrides` checks that the agents override file is valid
JSON and that those fields use the expected shapes.

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
- `aethyme enhance deploy --repo /path/to/repo`
- `aethyme enhance verify --repo /path/to/repo`
- `aethyme query symbol /path/to/repo main`
- `aethyme query deps /path/to/repo src/main.py`
- `aethyme query impact /path/to/repo src/main.py`

`enhance deploy` is the primary repo-facing discoverability path. It writes:
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
build `RepositoryMap` or mutate the store; run `aethyme-engine-cli index --repo
<repo>` first if the redb artifact is missing. For non-PHP scopes, or when
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
