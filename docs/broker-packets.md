# Aethyme Broker Packets

Status: **provisional v0 design**.
Audience: broker implementers, hook integrators, Chau7/MCP adapter authors, and
agent-surface consumers.

This document defines the expected shape of **broker packets**: compact,
deterministic workflow reports produced after noisy broker-adjacent actions such
as `git commit`, `git push`, `aethyme broker submit`, gate runs, repairs, and
resume/handoff checks.

Packets are intended to reduce token consumption. They should answer the
questions an agent usually spends context rediscovering:

- What action just happened?
- Did it pass, block, reject, warn, or need follow-up?
- Which session, files, gates, conflicts, or hooks matter?
- Where is the full log?
- What exact command should run next?

Packets are **not LLM summaries**. They are deterministic projections of
broker-owned facts: Git state, session state, gate selection, gate outcomes,
merge queue state, integration state, hook exit codes, logs, and configured
next commands.

## Stability Policy

This is a provisional v0 spec. It is intentionally exhaustive so the product
shape is clear, but it is **not yet a frozen JSON contract**.

Initial implementation should:

- keep packet construction in typed broker code, not in CLI-only rendering;
- allow additive field changes while v0 is proving itself;
- keep stable event and command contracts unchanged unless packets are
  explicitly promoted in `docs/events-contract.md` or `docs/json-contracts.md`;
- use snapshot tests for human rendering and focused shape tests for JSON.

When packet persistence becomes a long-lived integration surface, promote the
chosen fields into the frozen contract documents and add contract tests.

## Product Model

One broker action may have several delivery channels:

```text
broker-owned event happens
        |
        v
broker packet is created
        |
        +-- human stdout/stderr rendering, always when command owns the terminal
        +-- local log file, when there is command output or a failure
        +-- broker event/log row, when persistence is available
        +-- Chau7/MCP current-tab delivery, when an adapter can identify it
        +-- broker last/brief readback, when implemented
```

The packet is the source object. Stdout text, Chau7 messages, events, and later
`broker last`/`broker brief` views are renderings or transports of that same
object.

### Core Principles

- **Broker first, graph second, repo scan last.** Packets use broker-owned
  operational facts before asking Explore or semantic graph tools for context.
- **Facts over prose.** Packet summaries are generated from structured fields,
  not from model-written analysis.
- **Bounded by default.** Path lists, output tails, conflict lists, and gate
  lists are capped in compact renderings. The full logs remain available by
  path.
- **Portable core, optional adapters.** Chau7/MCP delivery is an adapter. The
  broker packet must remain useful through stdout/log/event fallback.
- **External hooks are opaque unless they cooperate.** Unknown third-party hook
  output is captured and linked; it is not parsed as authoritative structure.
- **Delivery failure is non-fatal.** Failing to push to Chau7/MCP must not
  change hook, gate, submit, or repair success/failure semantics.

## Canonical Packet Template

All fields below are part of the v0 design vocabulary. Implementations may omit
fields that are not available for a packet family, but they should use the same
names and meanings when the data is present.

```json
{
  "packet_version": 0,
  "packet_id": "local-persistent-id-or-null",
  "created_at_ms": 1785340000000,

  "action": "pre_commit",
  "status": "blocked",
  "severity": "blocked",
  "summary": "Aethyme pre-commit blocked on rust-format.",
  "reason_code": "gate_failed",

  "repo": {
    "root": "/Users/example/repo",
    "main_head": "0123456789abcdef0123456789abcdef01234567",
    "integration_branch": "aethyme/integration",
    "integration_head": "89abcdef0123456789abcdef0123456789abcdef",
    "integration_relation": "ahead_of_main"
  },

  "session": {
    "id": 42,
    "task": "Update broker gate runner",
    "worktree_path": "/Users/example/repo/.aethyme/worktrees/gate-runner",
    "branch": "agent/gate-runner",
    "diff_base": "0123456789abcdef0123456789abcdef01234567",
    "head": "fedcba9876543210fedcba9876543210fedcba98",
    "dirty": true,
    "dirty_paths": ["packages/aethyme/rust/crates/aethyme-broker/src/gates.rs"]
  },

  "inputs": {
    "staged_files": [
      "packages/aethyme/rust/crates/aethyme-broker/src/gates.rs"
    ],
    "head_changed_files": [],
    "outgoing_refs": [],
    "outgoing_files": [],
    "changed_since_base": [
      "packages/aethyme/rust/crates/aethyme-broker/src/gates.rs"
    ],
    "selected_gates": [
      {
        "name": "rust-format",
        "triggered_by": "packages/aethyme/rust/crates/aethyme-broker/src/gates.rs",
        "cost_tier": 1,
        "command": "cargo fmt --check"
      }
    ]
  },

  "result": {
    "gates": [
      {
        "name": "rust-format",
        "status": "fail",
        "cached": false,
        "exit_code": 1,
        "failure_class": "test_failure",
        "duration_ms": 412,
        "tree": null,
        "log_path": ".aethyme/logs/hooks/pre-commit-rust-format-814.log"
      }
    ],
    "conflicts": [],
    "external_hook": null
  },

  "log": {
    "primary_log_path": ".aethyme/logs/hooks/pre-commit-rust-format-814.log",
    "stdout_tail": [],
    "stderr_tail": [
      "Diff in packages/aethyme/rust/crates/aethyme-broker/src/gates.rs"
    ],
    "tail_line_limit": 20,
    "omitted": true
  },

  "next_actions": [
    {
      "label": "Format Rust sources",
      "command": "cargo fmt",
      "kind": "fix",
      "required": true
    },
    {
      "label": "Retry commit",
      "command": "git commit",
      "kind": "retry",
      "required": true
    }
  ],

  "delivery": {
    "stdout": true,
    "stderr": true,
    "event_recorded": true,
    "log_written": true,
    "chau7_attempted": false,
    "chau7_delivered": false,
    "delivery_errors": []
  },

  "limits": {
    "paths_shown": 12,
    "paths_total": 1,
    "conflicts_shown": 12,
    "conflicts_total": 0,
    "gates_shown": 8,
    "gates_total": 1,
    "tail_lines": 20
  }
}
```

## Field Reference

### Envelope

| Field | Required | Meaning |
|---|---:|---|
| `packet_version` | yes | Packet schema generation. v0 is provisional. |
| `packet_id` | no | Stable local id when the packet is persisted. Null/absent for transient render-only packets. |
| `created_at_ms` | yes | Unix epoch milliseconds when the packet was built. |
| `action` | yes | Workflow action that produced the packet. |
| `status` | yes | Outcome class. Determines exit/readback behavior. |
| `severity` | yes | Display urgency. Does not replace process exit code. |
| `summary` | yes | One deterministic sentence for compact display. |
| `reason_code` | yes | Machine-readable reason. Use stable snake_case. |

### `action`

Allowed v0 values:

| Value | Produced by |
|---|---|
| `pre_commit` | Aethyme-managed or Aethyme-wrapped Git pre-commit hook. |
| `post_commit` | Aethyme-managed or Aethyme-wrapped Git post-commit hook. |
| `pre_push` | Future user-owned push wrapper or external-hook adapter; Aethyme does not manage `pre-push`. |
| `submit` | `aethyme broker submit --session <id>`. |
| `gate_run` | `aethyme broker gates run --session <id>` or `--all`. |
| `repair` | `aethyme broker repair --session <id>`. |
| `status` | `aethyme broker status` compact packet rendering. |
| `brief` | Future `aethyme broker brief --session <id>`. |
| `last` | Future `aethyme broker last --session <id>`. |
| `external_hook` | Wrapper report for hook manager/user hook failure. |

### `status`

Allowed v0 values:

| Value | Meaning |
|---|---|
| `passed` | Action completed successfully with no follow-up required. |
| `blocked` | Action intentionally blocked local progress, usually a hook or dirty-state guard. |
| `rejected` | Submit/gate flow rejected promotion or verification. |
| `conflict` | Merge simulation, promoted layer, or lease overlap needs attention. |
| `warning` | Action completed but found coordination risk. |
| `info` | Informational packet only. |
| `error` | Broker could not complete the action due to environment, IO, or internal error. |

### `severity`

Allowed v0 values:

| Value | Meaning |
|---|---|
| `info` | No action required. |
| `notice` | Useful context; action may continue. |
| `warning` | Risk exists; user or agent should inspect before proceeding. |
| `blocked` | Action cannot proceed until required next action is handled. |

### Recommended `reason_code` Values

Reason codes should be specific enough for adapters to choose display behavior
without parsing `summary`.

| Reason code | Typical action | Meaning |
|---|---|---|
| `no_policy` | hooks/gates | No gates or broker DB configured; nothing ran. |
| `no_selected_gates` | pre_commit/gate_run | Gate config exists but no gate matched the input paths. |
| `all_gates_passed` | pre_commit/gate_run/submit | Selected gates passed. |
| `gate_failed` | pre_commit/gate_run/submit | A broker-owned gate failed. |
| `gate_error` | pre_commit/gate_run/submit | A gate could not execute or ended in an environment/internal error. |
| `gate_cached_prior_fail` | gate_run/submit | Cached failed result was reused. |
| `merge_conflict` | submit/repair | Merge simulation produced textual conflicts. |
| `promoted_conflict` | status/brief/repair | Session overlaps promoted integration work. |
| `live_overlap` | post_commit/status/brief | Session paths overlap live-session leases. |
| `dirty_worktree` | pre_push/submit/repair/brief | Worktree has uncommitted changes. |
| `unsubmitted_commits` | pre_push/finish/brief | Work exists locally but has not been broker-submitted. |
| `integration_may_move` | status/brief/pre_push | Live sessions may move integration while checks run. |
| `integration_ahead_main` | pre_push/status/brief | Local integration contains promoted work not yet in main. |
| `repair_rebased` | repair | Broker performed the documented local rebase. |
| `repair_noop` | repair | No current repair target exists. |
| `external_hook_failed` | external_hook | A non-Aethyme hook failed; broker captured only exit/log facts. |
| `delivery_failed` | any | Primary action completed, but adapter delivery failed. |
| `unknown` | any | Broker cannot classify more narrowly. |

### Repo Object

| Field | Required | Meaning |
|---|---:|---|
| `root` | no | Absolute repo root if discoverable. |
| `main_head` | no | Current main checkout `HEAD`. |
| `integration_branch` | no | Configured local integration branch, usually `aethyme/integration`. |
| `integration_head` | no | Current integration branch commit. |
| `integration_relation` | no | Relation between main and integration. |

`integration_relation` values:

- `current_with_main`
- `ahead_of_main`
- `diverged_from_main`
- `unknown`

### Session Object

| Field | Required | Meaning |
|---|---:|---|
| `id` | no | Broker session id if the current worktree is registered. |
| `task` | no | Session task text. Omit from telemetry that must avoid task text. |
| `worktree_path` | no | Absolute worktree path. |
| `branch` | no | Current/session branch. |
| `diff_base` | no | Session baseline commit. |
| `head` | no | Current worktree `HEAD`. |
| `dirty` | no | Whether uncommitted work exists. |
| `dirty_paths` | no | Bounded repo-relative dirty path list. |

Packets may exist without a session. Examples: hook manager failure before repo
discovery, `gates run --all`, or first-run diagnostics.

### Inputs Object

The inputs object records the deterministic input set that selected the work.
Different actions fill different fields.

| Field | Used by | Meaning |
|---|---|---|
| `staged_files` | `pre_commit` | Repo-relative paths from the Git index. |
| `head_changed_files` | `post_commit` | Repo-relative paths changed by `HEAD`. |
| `outgoing_refs` | `pre_push` | Parsed refs from Git pre-push stdin. |
| `outgoing_files` | `pre_push` | Repo-relative files touched by outgoing commit ranges. |
| `changed_since_base` | session actions | Repo-relative paths changed between `diff_base` and session `HEAD`. |
| `selected_gates` | gate actions | Gates selected by path triggers or always-run policy. |

Gate selection entries:

| Field | Required | Meaning |
|---|---:|---|
| `name` | yes | Gate name from `.aethyme/gates.toml`. |
| `triggered_by` | no | Path that selected this gate; null for always-run gates. |
| `cost_tier` | no | Gate cost tier. |
| `command` | no | Gate command. Include in local display, omit from privacy-restricted telemetry if needed. |

### Result Object

The result object records what happened.

#### Gate Result

| Field | Required | Meaning |
|---|---:|---|
| `name` | yes | Gate name. |
| `status` | yes | `pass`, `fail`, `error`, `cancelled`, or `cached`. |
| `cached` | yes | Whether execution was avoided by a cache hit. |
| `exit_code` | no | Process exit code, if known. |
| `failure_class` | no | Deterministic failure class, null for pass. |
| `duration_ms` | no | Run duration or cached saved duration where applicable. |
| `tree` | no | Git tree hash used as gate cache key, when available. |
| `log_path` | no | Full log path, preferably repo-relative under `.aethyme/logs/`. |

Allowed `failure_class` values match the broker gate model:

- `test_failure`
- `environment`
- `resource_contention`
- `timeout`
- `cached_prior_fail`
- `unknown`
- `null`

#### Conflict Result

| Field | Required | Meaning |
|---|---:|---|
| `path` | yes | Repo-relative path involved in the conflict or overlap. |
| `blocking_sessions` | no | Session ids implicated in the conflict. |
| `source` | yes | `live_lease`, `promoted_integration`, or `merge_simulation`. |

#### External Hook Result

| Field | Required | Meaning |
|---|---:|---|
| `hook` | yes | Hook name, such as `pre-commit` or `pre-push`. |
| `source` | yes | `core.hooksPath`, `user_hook`, `hook_manager`, or `unknown`. |
| `exit_code` | no | External process exit code. |
| `log_path` | no | Captured log path. |

External hook results are deliberately shallow. Unless the hook writes a
cooperative report, Aethyme must not classify individual lint/test failures
inside arbitrary third-party output.

### Log Object

| Field | Required | Meaning |
|---|---:|---|
| `primary_log_path` | no | Main log to inspect for full output. |
| `stdout_tail` | no | Bounded tail of stdout lines. |
| `stderr_tail` | no | Bounded tail of stderr lines. |
| `tail_line_limit` | yes when tails are present | Number of tail lines included per stream. |
| `omitted` | yes | True when full output exists but was not embedded. |

Default tail policy:

- include at most 20 stdout lines and 20 stderr lines in verbose local display;
- include fewer or none in compact display;
- never embed full command logs in packet display;
- always provide `primary_log_path` when output influenced failure diagnosis.

### Next Actions

| Field | Required | Meaning |
|---|---:|---|
| `label` | yes | Short display label. |
| `command` | no | Exact shell command to run, when a deterministic command exists. |
| `kind` | yes | Machine-readable next-action class. |
| `required` | yes | Whether the action gates progress. |

Allowed `kind` values:

- `fix`
- `retry`
- `inspect`
- `repair`
- `submit`
- `finish`
- `status`
- `wait`
- `commit`
- `push`
- `manual`

Rules:

- Required actions come before optional actions.
- Commands must be exact and copy-pasteable.
- Do not emit a fake command when the broker cannot know the fix.
- Prefer broker commands when the broker has authoritative recovery logic.

### Delivery Object

| Field | Meaning |
|---|---|
| `stdout` | Packet or compact rendering was printed to stdout. |
| `stderr` | Packet or compact rendering was printed to stderr. |
| `event_recorded` | Packet or related event was written to broker persistence. |
| `log_written` | Full output or packet details were written to a log. |
| `chau7_attempted` | Chau7/MCP adapter attempted delivery. |
| `chau7_delivered` | Chau7/MCP adapter reported success. |
| `delivery_errors` | Adapter errors. These must not alter primary action status. |

Delivery is about notification only. Hook and broker command exit behavior is
determined by the action result, not by whether delivery succeeded.

### Limits Object

The limits object makes bounded output explicit so agents know whether more
data exists.

| Field | Meaning |
|---|---|
| `paths_shown` / `paths_total` | How many paths are displayed vs known. |
| `conflicts_shown` / `conflicts_total` | How many conflicts are displayed vs known. |
| `gates_shown` / `gates_total` | How many gates are displayed vs known. |
| `tail_lines` | Tail lines included per stream. |

## Packet Families

### Pre-Commit Packet

Produced by Aethyme-managed or Aethyme-wrapped `pre-commit`.

Current support: Aethyme already manages a pre-commit hook that runs cost <= 1
gates selected by staged files.

Deterministic inputs:

- repo root from Git discovery;
- staged files from the Git index;
- `.aethyme/gates.toml`;
- gates with `cost <= 1`;
- gate selection by trigger match or always-run policy;
- gate command exit status;
- captured stdout/stderr/log path.

Required fields:

- `action: "pre_commit"`;
- `status: "passed"`, `"blocked"`, or `"error"`;
- `inputs.staged_files`;
- `inputs.selected_gates`;
- `result.gates` for each gate that ran or was skipped by cache;
- `log.primary_log_path` for failures;
- `next_actions` with retry command when blocked.

Exit behavior:

- pass when no config exists;
- pass when no cheap gate is selected;
- pass when all selected cheap gates pass;
- fail/block on first selected gate failure;
- fail/block on broken gate config because silently skipping broken policy is
  unsafe.

Compact blocked rendering:

```text
Aethyme pre-commit: blocked

Failed:
- gate: rust-format
- class: test_failure
- exit: 1
- files checked: 4
- log: .aethyme/logs/hooks/pre-commit-rust-format-814.log

Next:
- run: cargo fmt
- retry: git commit
```

Pass/no-op rendering:

```text
Aethyme pre-commit: passed
- selected gates: none
- files checked: 2
```

### Post-Commit Packet

Produced by Aethyme-managed or Aethyme-wrapped `post-commit`.

Current support: Aethyme already manages a post-commit conflict radar that
compares `HEAD` changed files with other live sessions' leases.

Deterministic inputs:

- repo root from Git discovery;
- files changed by `HEAD`;
- broker DB, when present;
- live sessions excluding this worktree;
- active leases;
- path-overlap comparison.

Required fields:

- `action: "post_commit"`;
- `status: "passed"` or `"warning"`;
- `inputs.head_changed_files`;
- `result.conflicts` with `source: "live_lease"` for overlaps;
- `next_actions` only when overlap exists.

Exit behavior:

- always exit 0;
- do not block a completed commit;
- warn only when overlap exists.

Compact warning rendering:

```text
Aethyme post-commit: warning

Overlap:
- session 88 also edits packages/aethyme/README.md

Next:
- run: aethyme broker status
```

### Pre-Push Packet

Produced by a future user-owned push wrapper or external-hook adapter that
explicitly calls into broker packet construction. Aethyme continues to manage
exactly `pre-commit` and `post-commit`; it does not install or own `pre-push`.

Current support: not implemented. This section defines target behavior for an
opt-in adapter, not an expansion of the broker-managed hook set.

Deterministic inputs:

- refs from Git pre-push stdin;
- remote name and URL from Git hook argv, if needed;
- local/remote commit ranges for each ref;
- outgoing commits and files;
- current worktree's broker session, if registered;
- dirty paths;
- latest queue entry for the session;
- integration relation;
- affected gates for outgoing/session changes if pre-push policy asks for gate
  proof.

Required fields:

- `action: "pre_push"`;
- `status: "passed"`, `"blocked"`, `"warning"`, or `"error"`;
- `inputs.outgoing_refs`;
- `inputs.outgoing_files`;
- current session and integration facts when discoverable;
- `next_actions` for submit/repair/status/retry.

Recommended blocking reasons:

- `dirty_worktree`: outgoing worktree has uncommitted changes and policy
  requires a clean push surface;
- `unsubmitted_commits`: current branch has commits not broker-submitted;
- `merge_conflict`: latest submit conflict still requires repair;
- `gate_failed`: required broker gate proof failed;
- `integration_ahead_main`: integration has promoted work absent from main and
  policy requires landing through integration first.

Exit behavior:

- block only on deterministic broker policy;
- warn, not block, for ambiguous state unless configured otherwise;
- pass when the repo is not broker-configured.

Compact blocked rendering:

```text
Aethyme pre-push: blocked

Reason:
- session 42 has unsubmitted commits
- integration: aethyme/integration is 2 commits ahead of main

Next:
- run: aethyme broker submit --session 42
- retry: git push
```

### Submit Packet

Produced by `aethyme broker submit --session <id>`.

Deterministic inputs:

- session id, task, worktree, branch, diff base, head;
- integration branch and head;
- merge-tree simulation result;
- conflict files and blocking sessions;
- affected gates and gate outcomes on the simulated merged tree;
- promote mode and queue status.

Required fields:

- `action: "submit"`;
- `status: "passed"`, `"rejected"`, `"conflict"`, or `"error"`;
- `session`;
- `repo.integration_branch` and `repo.integration_head`;
- `result.conflicts` when merge simulation conflicts;
- `result.gates` when gates ran;
- `next_actions`.

Outcome mapping:

| Broker outcome | Packet status | Typical reason code |
|---|---|---|
| promoted | `passed` | `all_gates_passed` |
| verified but manual promotion | `passed` | `all_gates_passed` |
| textual conflict | `conflict` | `merge_conflict` |
| gate rejected | `rejected` | `gate_failed` |
| environment/IO error | `error` | `gate_error` or `unknown` |

Compact conflict rendering:

```text
Aethyme submit: conflict

Conflicts:
- packages/aethyme/rust/crates/aethyme-broker/src/gates.rs
- packages/aethyme/rust/crates/aethyme-broker/src/cli.rs

Next:
- run: aethyme broker repair --session 42
- then: aethyme broker submit --session 42
```

Compact rejection rendering:

```text
Aethyme submit: rejected

Failed:
- gate: cargo-test
- class: test_failure
- log: .aethyme/logs/gates/cargo-test-9e685d05.log

Next:
- inspect: .aethyme/logs/gates/cargo-test-9e685d05.log
- run: aethyme broker gates run --session 42
- then: aethyme broker submit --session 42
```

### Gate-Run Packet

Produced by `aethyme broker gates run --session <id>` or
`aethyme broker gates run --all`.

Deterministic inputs:

- selected gates and trigger paths for session runs;
- all gates for `--all`;
- tree hash;
- cache lookup results;
- gate process exit status;
- log path and deterministic failure class.

Required fields:

- `action: "gate_run"`;
- `status: "passed"`, `"rejected"`, `"blocked"`, or `"error"`;
- `inputs.selected_gates`;
- `result.gates`;
- `log.primary_log_path` for first failing/error gate;
- `next_actions`.

Outcome mapping:

| Gate status | Packet status |
|---|---|
| all pass or cached pass | `passed` |
| fail/test failure | `rejected` |
| environment/resource/timeout/error | `error` |
| cancelled by obsolete run | `blocked` or `warning`, depending on caller |

Compact rendering:

```text
Aethyme gates: rejected

Failed:
- gate: pytest-local
- class: test_failure
- exit: 1
- log: .aethyme/logs/gates/pytest-local-f7d1a6dc.log

Next:
- inspect: .aethyme/logs/gates/pytest-local-f7d1a6dc.log
- retry: aethyme broker gates run --session 42
```

### Repair Packet

Produced by `aethyme broker repair --session <id>`.

Deterministic inputs:

- latest queue entry for session;
- latest submit conflict details;
- promoted-integration conflict detection;
- dirty paths;
- target base commit;
- affected gates after repair.

Required fields:

- `action: "repair"`;
- `status: "passed"`, `"blocked"`, or `"info"`;
- `reason_code`;
- repair source and target base, when present;
- refreshed `inputs.selected_gates` or affected gate summary;
- `next_actions`.

Outcome mapping:

| Repair result | Packet status | Reason |
|---|---|---|
| rebase performed | `passed` | `repair_rebased` |
| no repair target | `info` | `repair_noop` |
| dirty worktree blocks repair | `blocked` | `dirty_worktree` |
| rebase fails | `blocked` | `merge_conflict` |

Compact rendering:

```text
Aethyme repair: rebased session 42

Source:
- latest submit conflict
- base: 562ef8532718

Affected gates:
- cargo-test

Next:
- run: aethyme broker submit --session 42
```

### Status, Brief, And Last Packets

Produced by current or future read commands:

- `aethyme broker status`;
- future `aethyme broker brief --session <id>`;
- future `aethyme broker last --session <id>`.

These packets are readback views. They should not mutate repo state, except for
existing status-side lease refresh behavior where already established.

`status` answers: "What is the whole broker picture?"

`brief` answers: "What is this session's current compact operating state?"

`last` answers: "What just happened to this session or worktree?"

Required fields for `brief`:

- current session facts;
- changed files since `diff_base`;
- dirty paths;
- latest queue status;
- latest gate failure or conflict;
- live overlaps and promoted conflicts touching this session;
- integration relation;
- exact next action.

Compact brief rendering:

```text
Session 42: Update broker gate runner

State:
- base: 562ef8532718
- head: 4cff9b7c2060
- integration: 562ef8532718, current with main
- worktree: clean

Changed files:
- packages/aethyme/rust/crates/aethyme-broker/src/gates.rs
- packages/aethyme/rust/crates/aethyme-broker/tests/gates_e2e.rs

Latest result:
- submit rejected by cargo-test/test_failure
- log: .aethyme/logs/gates/cargo-test-9e685d05.log

Next:
- run: aethyme broker gates run --session 42
- then: aethyme broker submit --session 42
```

### External-Hook Fallback Packet

Produced when Aethyme wraps or is chained with a non-Aethyme hook that fails,
but the hook does not provide a cooperative report.

Deterministic inputs:

- hook name;
- hook source, if known;
- external process exit code;
- captured stdout/stderr log;
- current repo/session if discoverable.

Required fields:

- `action: "external_hook"`;
- `status: "blocked"` or `"error"`;
- `reason_code: "external_hook_failed"`;
- `result.external_hook`;
- `log.primary_log_path`;
- `next_actions` with inspect/retry only.

Compact rendering:

```text
Aethyme external hook: blocked

Failed:
- hook: pre-push
- source: core.hooksPath
- exit: 1
- log: .aethyme/logs/hooks/pre-push-external-123.log

Next:
- inspect: .aethyme/logs/hooks/pre-push-external-123.log
- retry: git push
```

## Cooperative External Hook Reports

Existing hooks do not need to change. However, hook managers or repo scripts may
later opt into richer structure by writing JSON to a path provided by Aethyme.

Proposed environment variable:

```text
AETHYME_HOOK_REPORT=.aethyme/run/hooks/pre-push-report.json
```

Proposed cooperative report:

```json
{
  "tool": "eslint",
  "status": "failed",
  "summary": "2 lint errors",
  "files": ["src/app.ts", "src/routes.ts"],
  "failure_class": "test_failure",
  "log_path": ".aethyme/logs/hooks/eslint-123.log",
  "next_actions": [
    {
      "label": "Fix lint",
      "command": "npm run lint -- --fix",
      "kind": "fix",
      "required": true
    }
  ]
}
```

Rules for cooperative reports:

- Aethyme may include cooperative fields in the packet, but must still record
  the external process exit code.
- Aethyme must validate report JSON before trusting it.
- Aethyme must tolerate missing, malformed, or partially written reports.
- Cooperative reports may add detail, but they do not override broker-owned
  facts such as session id, integration head, or gate cache status.

## Chau7/MCP Delivery

Chau7/MCP delivery is a packet adapter.

Target behavior:

- identify the requesting Chau7 tab/session from MCP metadata, environment, or
  an explicit delivery target;
- send the compact rendering to that tab/session;
- record delivery attempt/success/failure in `delivery`;
- fall back to stdout/log/event when delivery is unavailable.

Required adapter properties:

- no broker core logic inside the adapter;
- no workflow failure caused solely by adapter failure;
- bounded output identical in meaning to stdout compact rendering;
- no hidden expansion of full logs unless explicitly requested by the user or
  agent.

Delivery correlation candidates, in preferred order:

1. MCP request metadata naming the caller tab/session.
2. Environment variables injected by Chau7 when running a command.
3. Explicit CLI flag or config value for delivery target.
4. No adapter delivery; stdout/log/event fallback only.

## Rendering Rules

Compact rendering should be short and stable:

- first line: `Aethyme <action>: <status>`;
- then one or more short sections: `Failed`, `Conflicts`, `Warnings`, `State`,
  `Log`, `Next`;
- include exact commands prefixed with `run:`, `retry:`, `then:`, or `inspect:`;
- show bounded path lists with an `and N more` line when capped;
- use repo-relative paths when possible;
- include short commits in human output and full commits in JSON;
- avoid terminal-color dependency;
- avoid non-deterministic wording.

Verbose rendering may include:

- larger path lists;
- stdout/stderr tails;
- all selected gates;
- all outgoing refs;
- delivery diagnostics.

Full log output should remain out-of-band and referenced by path.

## Determinism Checklist

Packet construction must obey these rules:

- Sort paths lexicographically unless preserving Git-provided order is required
  for correctness.
- Sort session/conflict summaries by session id, then path.
- Sort selected gates by broker gate order.
- Stop pre-commit at the first failing gate unless policy later changes.
- Preserve exact exit codes.
- Preserve exact log paths.
- Use known failure classes only when the broker owns classification.
- Use `unknown` rather than guessing.
- Do not parse arbitrary external hook output as a semantic source of truth.
- Do not call Explore/graph tools in hook paths.
- Do not include full logs in packets.
- Do not let delivery errors change action status.

## Privacy And Telemetry

Local stdout and local logs may include task text, paths, and commands because
they are already visible to the local workflow.

Metrics and privacy-restricted exports must not include:

- task text;
- arbitrary file paths;
- full commands;
- stdout/stderr content.

If a packet is exported beyond the local repo, the exporter must explicitly
choose a privacy tier and redact fields accordingly.

## Implementation Notes

Recommended implementation sequence:

1. Add typed packet structs in the broker crate.
2. Add packet builders for gate outcomes and submit outcomes.
3. Update `pre-commit` to capture gate output to logs and render a packet.
4. Update `post-commit` conflict radar to render a packet instead of ad hoc
   warning lines.
5. Add an optional packet API for user-owned push wrappers and external-hook
   adapters without expanding the broker-managed hook set.
6. Add persistence/readback for `broker last`.
7. Add session-scoped `broker brief`.
8. Add Chau7/MCP delivery adapter.
9. Promote the proven JSON/event shape to stable contracts only after dogfood.

Suggested module boundaries:

- broker library creates packet structs and next-action decisions;
- hooks module supplies Git hook inputs and exit behavior;
- gates/merge modules supply gate and submit outcome facts;
- CLI renders packets;
- adapters deliver packets to Chau7/MCP or future surfaces.

## Test Scenarios

Implementation should add tests for:

- pre-commit with no `.aethyme/gates.toml` produces `no_policy` pass;
- pre-commit with gates config but no selected gates produces
  `no_selected_gates` pass;
- pre-commit with one failing cheap gate produces `gate_failed` blocked packet,
  exit code, failure class, log path, and retry command;
- pre-commit with broken gate config produces blocked/error packet rather than
  silent pass;
- post-commit with no broker DB is silent or info-only and exits 0;
- post-commit with live overlap produces warning packet and exits 0;
- a user-owned pre-push adapter fixture parses stdin into outgoing refs/files;
- a user-owned pre-push adapter blocks unsubmitted session commits when configured;
- submit conflict packet includes bounded conflict paths and repair/submit next
  commands;
- submit rejected packet includes failing gate, failure class, log path, and
  gate retry command;
- gate-run cached prior fail is reported as cached with `cached_prior_fail`;
- repair dirty-worktree packet blocks before rebase;
- repair success packet reports target base and submit next command;
- external hook failure packet preserves hook name, source, exit code, and log
  path without parsing output;
- malformed cooperative hook JSON falls back to external-hook packet;
- Chau7 delivery success/failure only changes `delivery`, not action status;
- compact human rendering is stable under snapshot tests;
- provisional JSON shape test is separate from frozen contract tests.

## Relationship To Existing Contracts

Existing broker contracts remain authoritative:

- `docs/events-contract.md` defines the frozen event stream.
- `docs/json-contracts.md` defines frozen command JSON surfaces.
- Broker packet JSON is provisional until explicitly promoted.

Where packet data overlaps an existing contract, implementations should reuse
the same vocabulary:

- gate statuses and failure classes from broker gate results;
- merge statuses from the merge queue;
- session fields from broker sessions;
- integration relation names from status/integration views where practical;
- event ids and schema versions only when packet persistence is implemented.
