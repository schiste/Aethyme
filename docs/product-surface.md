# Aethyme Public Product Surface

Status: current public surface, 2026-07-29

Aethyme is a local-first CLI for coordinating AI coding agents and for
answering bounded repository-navigation questions. The product surface is
broker-first: the graph engine is a supporting intelligence layer, not the
front-door product.

## Canonical User Journeys

### 1. Onboard A Repo

Use this when adding Aethyme to a repository for the first time.

```bash
cargo install --path packages/aethyme/rust/crates/aethyme-engine
cd /path/to/repo
aethyme init
aethyme broker quick-test
aethyme broker verify-loop
```

What this proves:

- `aethyme init` certifies the repo, scaffolds `.aethyme/`, and drafts gates
  when manifests make that possible.
- `aethyme broker quick-test` proves the local broker loop in a disposable
  repo without touching the target repo.
- `aethyme broker verify-loop` snapshots the integration tip, runs the smoke,
  checks doctor output, and reports if integration moved during the run.

### 2. Coordinate Agent Work

Use this as the daily operator loop when agents work concurrently.

```bash
aethyme broker status
aethyme broker start --task "Describe the task"
aethyme broker leases claim <path> --session <id>
aethyme broker exec --session <id> -- <command>
# edit and commit in the session worktree
aethyme broker submit --session <id>
aethyme broker repair --session <id>
aethyme broker finish --session <id>
aethyme broker integration status
```

What this provides:

- broker-created worktrees, with `adopt` available for existing worktrees
- dirty-worktree visibility, explicit leases, and overlap warnings
- guarded command execution for broad rewrites
- merge simulation before promotion
- repo-owned gates on the merged tree
- promoted-but-unmerged integration visibility
- written recovery steps for rejected submits

### 3. Navigate A Repo

Use this when an agent needs a bounded answer about where to look.

```bash
aethyme explore --repo /path/to/repo --request "Find the files responsible for this behavior" --format answer-json
aethyme intents --request "Find public functions with no outside callers" --format compact-json
aethyme graph callers /path/to/repo <target> --json-output
aethyme task pack --repo /path/to/repo --task "Explain this area" --json-output
```

What this provides:

- deterministic candidate files, symbols, call sites, and next-step targets
- observability about graph freshness, completeness, and answer safety
- lower-level graph and task-pack commands for power users

## Command Tiers

### Stable Front Door

These commands are the public product path and should stay easy to explain:

- `aethyme init`
- `aethyme certify`
- `aethyme broker status`
- `aethyme broker start`
- `aethyme broker adopt`
- `aethyme broker exec`
- `aethyme broker submit`
- `aethyme broker repair`
- `aethyme broker finish`
- `aethyme broker integration status`
- `aethyme broker quick-test`
- `aethyme broker verify-loop`
- `aethyme explore`

### Advanced Public Tools

These are public, but they are power-user or integration surfaces rather than
the first-time story:

- `aethyme broker gates ...`
- `aethyme broker events`
- `aethyme broker metrics`
- `aethyme broker doctor`
- `aethyme broker leases ...`
- `aethyme broker cleanup`
- `aethyme graph ...`
- `aethyme facts ...`
- `aethyme task ...`
- `aethyme analyze dead-code`
- `aethyme enhance deploy`
- `aethyme enhance verify`
- `aethyme repo experience-*`

### Internal Or Historical

These should not lead product docs unless they are being actively promoted:

- local eval harness commands
- benchmark/report generation scripts
- graph storage implementation details
- old SaaS/API/Kubernetes material
- compatibility commands kept only for migration

## Confidence Commands

`quick-test` and `verify-loop` are the operator confidence commands.

`aethyme broker quick-test` is the install smoke. It creates a disposable repo,
runs init, adopts a session, commits a broker-owned change, submits it, verifies
promotion, and removes the temporary repo.

`aethyme broker verify-loop` is the fuller broker E2E. It reports the
integration commit tested and fails if integration moved before the result could
prove the current tip. Inside the Aethyme source checkout, it may also run
focused broker source tests.

## JSON Stability

Only the frozen broker JSON contracts in
[`json-contracts.md`](json-contracts.md) are stable for long-lived scripts:

- `aethyme broker status --json`
- `aethyme broker integration status --json`
- `aethyme broker events --json`
- `aethyme broker metrics --json`
- `aethyme broker submit --json`

Other JSON outputs are useful operationally but provisional until they are
promoted into that contract document. In particular, `quick-test`, `verify-loop`,
`doctor`, `certify`, `agents`, `adopt`, `leases`, and `gates` JSON should be
treated as best-effort.

## Broker As Repo Cleanliness Infrastructure

The broker can become a repo-cleanliness layer because it sees work as
sessions, diffs, files, commits, gates, and outcomes over time.

Highest-value improvements:

- **Structure advice in status.** Summarize active work by area, overlap risk,
  stale sessions, unsubmitted commits, and promoted-but-unmerged files.
- **Ownership-free hygiene signals.** Detect recurring conflict paths, noisy
  generated files, oversized commits, high-churn modules, and gates that fail
  only after merge simulation.
- **Session finish discipline.** Make `finish` the clean closure path: close
  clean sessions, warn on unsubmitted commits, suggest cleanup only when safe.
- **Gate policy drift.** Compare changed files and observed failures against
  `.aethyme/gates.toml`; suggest missing or overbroad gate triggers.
- **Repo protocol health.** Certify `AGENTS.md`, `.aethyme/config.toml`,
  `.aethyme/gates.toml`, gitignore blocks, and generated onboarding freshness.

This should stay advisory first. The broker should make the repo easier to keep
clean, not become an opaque policy engine that blocks work for subjective style
reasons.

## Broker As Token-Budget Infrastructure

The broker can reduce token consumption because it already tracks the state an
agent usually spends tokens rediscovering: current task, changed files, dirty
state, conflicts, gate history, integration head, and recent outcomes.

Highest-value improvements:

- **Session briefing.** Generate a compact, task-scoped briefing from broker
  state: base commit, changed files, related promoted work, conflicts, failing
  gates, and next command.
- **Delta context.** Prefer "what changed since your base" over broad repo
  summaries. Agents should start from the session diff and integration delta,
  then ask Explore only for missing context.
- **Failure memory.** Reuse gate logs and prior rejected-submit summaries so the
  next agent sees the exact failing command, failure class, and relevant paths
  without rereading long logs.
- **Conflict-aware retrieval.** When overlap or promoted conflicts exist,
  provide the small path set and rebase instructions first; do not prompt the
  agent to scan the whole repo.
- **Budgeted command outputs.** Broker-generated briefings should include
  bounded snippets and paths, with pointers to full logs/events for explicit
  follow-up.

The target product behavior is: before an agent reads the repo, it asks the
broker for the smallest current-state packet that can steer the next action.
