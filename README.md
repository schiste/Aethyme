# Aethyme

**Flight control for AI coding agents.** Aethyme is a local-first broker that
lets many concurrent AI agents (Claude Code, Codex, Aider, plain shell
scripts — any vendor) work on the same repository without colliding: each
agent flies in its own git worktree, files a task, and requests clearance to
land. The broker simulates the merge, runs your repo's checks on the merged
tree, and promotes verified work onto a local integration branch — or hands
the agent precise rebase instructions when it conflicts.

No cloud, no daemon, no dashboard: a single Rust binary coordinating through
SQLite in `.aethyme/`, on macOS and Linux.

## The flight-control model

| Airport | Aethyme | What it does |
| --- | --- | --- |
| **The tower** | the broker (`aethyme broker ...`) | Tracks every active session (worktree + branch + task), warns when two agents edit the same files, serializes landings on the integration branch. |
| **Clearance to land** | `submit` → simulate → gates → promote | A submission is merge-simulated against the integration branch *before* anything runs. Conflicts are rejected in milliseconds with written recovery steps; clean merges get your checks run on the merged tree — the only place semantic conflicts are detectable — then promote. |
| **Regulations** | `.aethyme/gates.toml` | Quality rules are repo policy, not broker policy. You declare gates (command, cost tier, path triggers); the broker selects the affected ones, runs cheap-first, caches by tree hash, and cancels obsolete runs. A repo with no gates is a valid conflict-only deployment. |
| **Certification** | `aethyme init` / `aethyme certify` | `certify` is a read-only inspection (git version, repo state, config validity, gitignore contract, database integrity) you can run in CI or cron. `init` runs certify, then scaffolds only what's missing, then drafts gates from your manifests. Idempotent: a second run changes nothing and says so. |
| **Preflight smoke** | `aethyme broker quick-test` | Creates a disposable repo, runs init → adopt → commit → submit, verifies promotion, then removes the repo. Use it after install/init to prove the local broker loop before adopting real work. |
| **Verification loop** | `aethyme broker verify-loop` | Operator E2E: snapshots the integration head, runs quick-test, runs doctor, runs broker source tests when invoked inside the Aethyme checkout, and fails if integration moved before the result could prove the current tip. Alias: `aethyme broker e2e`. |
| **Charts** | the graph engine | A deterministic Rust repo-intelligence engine (indexing, graph navigation, impact frontiers, task-context packs). Today it serves queries on its own; feeding impact hints to the tower is planned, deliberately deferred. |
| **The flight recorder** | `aethyme broker events` | Every mutation appends to a versioned event log ([`docs/events-contract.md`](docs/events-contract.md)) — the integration contract for any future surface (TUI, editor plugin). |

Two design commitments underneath all of it:

- **Attach-first.** A session is an existing worktree you register
  (`broker adopt`), not a process the broker owns. Agents from different
  vendors coordinate because sessions are vendor-agnostic worktrees, not
  because of per-vendor adapters.
- **API-first.** The broker core is a typed Rust library; the CLI is a thin
  client and every command has a `--json` form. The promotion lands on a
  *local* integration branch only — the broker never pushes, never opens
  PRs; your GitHub flow stays human and unchanged.

## Current state — honest version

**v0 (MVP) is built and proven in dogfood.** Sessions, diff-derived leases
with overlap warnings, the affected gate runner with tree-hash caching,
merge simulation, auto-promotion, conflict hand-back
(`.aethyme/broker-action-required.md`), the append-only event log, managed
git hooks, `init`/`certify`, and cost/benefit `metrics` all work today.
Aethyme's own development runs through the broker — multi-agent batches on
this very repository, including a live-caught semantic conflict (two
sessions whose changes merged cleanly but broke together; the gate on the
merged tree caught it). Friction and cost accounting are logged in
[`docs/dogfood-friction.md`](docs/dogfood-friction.md).

**V1 is in progress.** The dogfood week is hardening the loop; known edges:
design ceiling of 15 concurrent sessions (stress-tested at 20), macOS/Linux
only, overlap warnings never block (v0 policy), and graph-aware broker
features (impact-hint advisories) are deferred. The deterministic graph
engine remains a separate supporting service.

**Removed.** Earlier cloud/SaaS work was deleted in 2026-07; no cloud
execution, auth, or team sync is part of the product. Direction doc:
[`docs/aethyme-local-agent-broker.md`](docs/aethyme-local-agent-broker.md).

## Quickstart: install -> init -> quick-test -> adopt -> submit

Prerequisites: git ≥ 2.38, a Rust toolchain, ~2 GB free RAM for the one-time compile (the bundled SQLite build is memory-hungry; small VMs/containers may OOM — prebuilt release binaries avoid the compile entirely), and any repo to try it on.

First-time flow: install -> `aethyme init` -> `aethyme broker quick-test` ->
`aethyme broker adopt` -> `aethyme broker submit --session <id>`.

**1. Install the binary** (from a clone of this repository):

```bash
cargo install --path packages/aethyme/rust/crates/aethyme-engine
```

**2. Certify and scaffold your target repo:**

```bash
cd /path/to/your-repo
aethyme init
```

```text
Phase 1/3 — certify (read-only):
pass     certify.git-version          git 2.55.0 (≥ 2.38 required for merge simulation)
pass     certify.git-repo             inside a git repository
pass     certify.head-commit          repository has at least one commit
pass     certify.binary-path          the running aethyme is the one on PATH
warn     certify.gates                no gates.toml — broker runs conflict-only (no verification); `aethyme broker gates draft` can draft one
...

Phase 2/3 — scaffold (deterministic, only-if-missing):
created  scaffold.config-toml         .aethyme/config.toml written — review the draft
created  scaffold.gitignore           appended the aethyme-broker block to .gitignore
created  scaffold.broker-db           integrity: ok

Phase 3/3 — gates draft (adaptive):
warn     gates.draft                  no manifests recognized — define .aethyme/gates.toml yourself; until then the broker runs conflict-only (no verification)

First-time flow: install -> `aethyme init` -> `aethyme broker quick-test` -> `aethyme broker adopt` -> `aethyme broker submit --session <id>`.
Next steps: review any drafts above, re-check anytime with `aethyme certify`, then run the disposable smoke before adopting real sessions; optionally `aethyme enhance deploy` installs the agent protocol into AGENTS.md/CLAUDE.md.
```

(On a repo with a `Cargo.toml`, `go.mod`, `package.json` scripts, or a
`pyproject.toml` mentioning pytest/ruff, phase 3 drafts a `gates.toml` for
you to review.) Commit the scaffold:

```bash
git add .gitignore .aethyme/config.toml
git commit -m "chore: adopt aethyme broker (scaffold)"
```

**3. Run the disposable broker smoke** — this creates and removes a temporary
repo; it does not touch your target repo:

```bash
aethyme broker quick-test
```

```text
broker quick test passed
pass     create-temp-repo     /tmp/aethyme-broker-quick-test-...
pass     git-bootstrap        initial commit on main
pass     aethyme-init         scaffold committed
pass     broker-adopt         session 1
pass     smoke-commit         committed one broker-owned change
pass     broker-submit        entry 1 promoted
temporary repo removed: yes
integration head: 69d395da1c7c
```

**4. Register a session** — here the current checkout; agents normally each
get their own worktree:

```bash
aethyme broker adopt --task "Add a farewell function"
```

```text
Adopted session 1 — worktree /private/tmp/demo-app on branch main
note: main-checkout session — verification is advisory here (commits land on main before gates run); use a worktree session for enforced verification.
```

**5. Do the work and commit it.** Only committed work integrates:

```bash
# ...edit src/app.py...
git add src/app.py
git commit -m "feat: add farewell function"
```

**6. Submit — simulate, gate, land:**

```bash
aethyme broker submit --session 1
```

```text
Submitting session 1 — HEAD 6613f5ccb1a0
  6613f5c feat: add farewell function
gate wall time: 0ms
entry 1 → promoted (auto-promoted)
What now: aethyme/integration is at 69d395da1c7c and contains this work. Your checkout and branches are untouched — keep working, or start a follow-up with `aethyme broker adopt --reuse --task "..."`, or finish safely with `aethyme broker finish --session 1`.
```

**7. What now?** Your promoted work is on the local `aethyme/integration`
branch (`git log aethyme/integration`); merge or push it through your normal
review flow whenever you choose. `aethyme broker status` shows the whole
picture:

```text
Integration: aethyme/integration @ 69d395da1c7c

ID   STATUS   ORIGIN   BRANCH                   TASK
1    active   adopted  main                     Add a farewell function

QID  SID  QSTATUS     HEAD
1    1    promoted    6613f5ccb1a0
```

From here: add a `gates.toml` so submissions are verified, not just
conflict-checked; add the Broker Coordination protocol to your `AGENTS.md`
so agents follow the loop unprompted (`aethyme certify` reports it as
`certify.agents-protocol`); and run the full two-agent conflict scenario in
[`docs/demo-script.md`](docs/demo-script.md) — worth five more minutes to
see the tower actually direct traffic.

## Repository layout

- `packages/aethyme/rust`: the Rust workspace — `aethyme-engine` (graph
  engine + the `aethyme` router binary), `aethyme-broker` (the broker
  library and CLI), and the graph schema/storage/indexer crates.
- `packages/aethyme`: the Python package (indexing, search, task context;
  the Rust router delegates non-broker commands to it). See
  [`packages/aethyme/README.md`](packages/aethyme/README.md) and
  [`packages/aethyme/rust/README.md`](packages/aethyme/rust/README.md).
- `docs`: project-level direction and contracts
  ([`docs/aethyme-local-agent-broker.md`](docs/aethyme-local-agent-broker.md),
  [`docs/events-contract.md`](docs/events-contract.md),
  [`docs/demo-script.md`](docs/demo-script.md)).

For graph-engine usage (indexing, `explore`, task-context packs) see the
longer guide at
[`packages/aethyme/docs/getting-started/quickstart.md`](packages/aethyme/docs/getting-started/quickstart.md).

## Development

```bash
cd packages/aethyme
python3 -m venv .venv && . .venv/bin/activate && pip install -e '.[dev]'
.venv/bin/pytest -q tests/local
cd rust && cargo test --workspace
```

This repository dogfoods its own broker — see the Broker Coordination
section in [`CLAUDE.md`](CLAUDE.md) / [`AGENTS.md`](AGENTS.md). Evaluation
work follows the Cardinal Rules there: evals run only against Playground
repositories, and the tool is never tuned to a score.

## Security and support

- Security policy: [`SECURITY.md`](SECURITY.md)
- Support scope: [`SUPPORT.md`](SUPPORT.md)
- Contribution guide: [`CONTRIBUTING.md`](CONTRIBUTING.md)
- Governance: [`GOVERNANCE.md`](GOVERNANCE.md)
- Code of conduct: [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md)

## License

Apache License 2.0. See [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE).
