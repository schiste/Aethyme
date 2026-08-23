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
| **Preflight smoke** | `aethyme broker quick-test` | Creates a disposable repo, runs init → adopt → commit → submit, verifies promotion, then removes the repo. Use it after install/init to prove the local broker loop before starting real work. |
| **Verification loop** | `aethyme broker verify-loop` | Operator E2E: snapshots the integration head, runs quick-test, runs doctor, runs broker source tests when invoked inside the Aethyme checkout, and fails if integration moved before the result could prove the current tip. Alias: `aethyme broker e2e`. |
| **Charts** | the graph engine | A deterministic Rust repo-intelligence engine (indexing, graph navigation, impact frontiers, task-context packs). Its bounded incoming-caller frontier also supplies optional hints to `broker gates semantic`; those suggestions never expand enforced gates. |
| **The flight recorder** | `aethyme broker events` | Every mutation appends to a versioned event log ([`docs/events-contract.md`](docs/events-contract.md)) — the integration contract for any future surface (TUI, editor plugin). |

Two design commitments underneath all of it:

- **Worktree-first.** The normal path is `broker start --task`, which creates
  an isolated worktree + branch and registers the session. `broker adopt`
  remains the attach path for an existing dedicated worktree. Agents from
  different vendors coordinate because sessions are vendor-agnostic
  worktrees, not because of per-vendor adapters.
- **API-first.** The broker core is a typed Rust library; the CLI is a thin
  client and every command has a `--json` form. `broker submit` promotes to a
  *local* integration branch only; explicitly authorized remote changes use
  the durable `broker git` / `broker gh` coordinators instead of bypassing the
  broker.

For the current public product map, including the three canonical user
journeys, command tiers, confidence commands, JSON stability, and the next
broker directions for repo cleanliness and token budgets, see
[`docs/product-surface.md`](docs/product-surface.md).
For the safe follow-up paths after a first submission—including session reuse,
fresh gate evidence, lease preflight, and durable finish handoffs—see
[`packages/aethyme/docs/guides/broker-workflows.md`](packages/aethyme/docs/guides/broker-workflows.md).

## Current state — honest version

**v0 (MVP) is built and proven in dogfood.** Sessions, diff-derived leases
with overlap warnings, explicit write leases, guarded command execution, the
affected gate runner with tree-hash caching, merge simulation, auto-promotion,
conflict hand-back
(`.aethyme/broker-action-required.md`), the append-only event log, managed
git hooks, `init`/`certify`, and cost/benefit `metrics` all work today.
Aethyme's own development runs through the broker — multi-agent batches on
this very repository, including a live-caught semantic conflict (two
sessions whose changes merged cleanly but broke together; the gate on the
merged tree caught it). Friction and cost accounting are logged in
[`docs/dogfood-friction.md`](docs/dogfood-friction.md).

**V1 is active.** The dogfood week and issue #33 closed on 2026-07-17 after
the broker passed its MVP exit checklist under real multi-agent load. Known edges:
design ceiling of 15 concurrent sessions (stress-tested at 20), macOS/Linux
only, implicit overlap warnings are advisory while explicit leases block, and
graph-aware gate hints are exposed only through the read-only
`broker gates semantic` report. The deterministic graph engine remains a
separate supporting service; changed-path triggers still exclusively control
`gates run` and submit-time verification.

**Removed.** Earlier cloud/SaaS work was deleted in 2026-07; no cloud
execution, auth, or team sync is part of the product. Direction doc:
[`docs/aethyme-local-agent-broker.md`](docs/aethyme-local-agent-broker.md).

## Quickstart: install -> init -> quick-test -> start -> submit

Prerequisites: git ≥ 2.38, a Rust toolchain, ~2 GB free RAM for the one-time compile (the bundled SQLite build is memory-hungry; small VMs/containers may OOM — prebuilt release binaries avoid the compile entirely), and any repo to try it on.

First-time flow: install -> `aethyme init` -> `aethyme broker quick-test` ->
`aethyme broker start --task "..."` -> `aethyme broker submit --session <id>`.

**1. Install the binary** (from a clone of this repository):

```bash
cargo install --path packages/aethyme/rust/crates/aethyme-cli
cargo install --path packages/aethyme/rust/crates/aethyme-engine  # engine-daemon sibling
```

Releases do not auto-update existing installations. To upgrade, check out the
desired release and rerun both `cargo install --path` commands above, then run
`aethyme --version`. The router and engine-daemon sibling should be upgraded
together; Aethyme does not silently mutate installed binaries in the background.

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

First-time flow: install -> `aethyme init` -> `aethyme broker quick-test` -> `aethyme broker start --task "..."` -> `aethyme broker submit --session <id>`.
Next steps: review any drafts above, re-check anytime with `aethyme certify`, then run the disposable smoke before starting real sessions; optionally `aethyme enhance deploy` installs the agent protocol into AGENTS.md/CLAUDE.md.
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

**4. Start a broker-managed session** — this creates an isolated worktree and
branch for the task:

```bash
aethyme broker start --task "Add a farewell function"
```

```text
Started session 1 — worktree /private/tmp/demo-app/.aethyme/worktrees/add-a-farewell-function on branch agent/add-a-farewell-function
Next: cd /private/tmp/demo-app/.aethyme/worktrees/add-a-farewell-function
```

`broker adopt --task "..."` is still available when you have already created
a dedicated worktree yourself.

**5. Do the work and commit it.** Only committed work integrates:

```bash
aethyme broker leases claim src/app.py --session 1
# ...edit src/app.py, or run broad tools through:
# aethyme broker exec --session 1 -- <command>
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
# Product: no Python anywhere.
cargo install --path packages/aethyme/rust/crates/aethyme-cli
cargo install --path packages/aethyme/rust/crates/aethyme-engine

# Tests: no Python there either, since 2026-08-06.
cd packages/aethyme/rust && cargo test --workspace
```

> **`python -m src.cli` no longer exists.** The Python package was deleted
> on 2026-08-01 (python-retirement Phase 6) and there is no shim: the old
> spelling now fails with `No module named src`. Every command is native —
> run `aethyme --help`. Installing the router is `cargo install`, with no
> interpreter, virtualenv, or pip step on the product path.

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
