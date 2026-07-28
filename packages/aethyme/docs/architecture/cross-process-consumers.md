# Cross-process consumers of Aethyme entry points

Last Updated: 2026-07-17

When code outside the `packages/aethyme/` Python or Rust source tree
invokes an Aethyme command, it crosses a process boundary. Static
analysis (ruff, cargo check, type checkers) does not see those
invocations. They have to be audited by hand.

This file is the canonical inventory: every shell script, hook, skill
template, deployed wrapper, and CI step that calls an Aethyme entry
point. **Before deleting or renaming any CLI entry point — Python
`cli.py` command, Rust binary subcommand, or shell helper — grep this
file for callers and update each.**

The 2026-05-08 hard-delete of `python -m src.cli explore` broke the
deployed `aethyme-explore` wrapper because we hadn't tracked it as a
consumer. This file exists so that doesn't happen again.

## How to use this file

1. **Adding a new wrapper / hook / external invocation?** Add a row
   to the appropriate section below. Note the source path, the
   invoked Aethyme entry point, and what kind of failure surfaces if
   the entry point disappears.

2. **Removing or renaming an Aethyme entry point?**
   - Search this file for the entry point's name.
   - For each consumer listed: either update the consumer in the same
     commit, or add a backwards-compat shim, or accept the breakage
     with explicit reasoning.
   - If you find a consumer that's NOT listed here, add it before
     proceeding. The miss is itself the bug.

3. **Suspect something is stale?** Run
   `scripts/eval/verify-playground.sh` against any deployed
   playground; it asserts a subset of the contracts below.

## Inventory

### Skill template files (canonical sources, deployed by `enhance.py` / `indexing/skills.py`)

| Source | Deployed to | Invokes | Failure mode if entry point removed |
|---|---|---|---|
| `skills/aethyme/SKILL.md` | `.claude/skills/aethyme/SKILL.md`, `.codex/skills/aethyme/SKILL.md` | Short auto-load card: one bounded native `aethyme explore` call, inspect trust/observability, then verify narrowly. Links detailed workflows under `references/`. | Agent reads stale guidance, runs commands that no longer exist, or bulk-loads detailed workflows. Caught by `verify-playground.sh` greps, `test_skill_progressive_disclosure.py`, and `scripts/check-cross-process-contract.py` text-consumer validation. |
| `skills/aethyme/references/*.md` | `.claude/skills/aethyme/references/*.md`, `.codex/skills/aethyme/references/*.md` | Detailed optional workflows: Explore depth/intents/trust, graph/task/context commands, and dead-code/facts/analyzer commands. | Enhanced repos receive a short skill with broken reference links or stale detailed commands. Caught by `enhance verify`, `verify-playground.sh`, and reference deployment tests. |
| `skills/aethyme/AGENTS.md` | fully generated `AGENTS.md` and `CLAUDE.md` (deployed at repo root by `enhance.py`) | Cross-product convention file with quick-start command guidance, compact repo routing from generated onboarding/status artifacts, and commit hygiene policy. Root files are Aethyme-owned generated artifacts; repo-specific human customizations come from `.aethyme/overrides/agents.json`. | Agent reads stale quick start before loading skill details. Caught by `scripts/check-cross-process-contract.py` text-consumer validation and `enhance verify` canonical-match checks. |
| `skills/aethyme/aethyme-explore` | `.codex/skills/aethyme/aethyme-explore` (executable) | `exec "{{AETHYME_ROOT}}/rust/target/release/aethyme" explore "$@"` | Wrapper produces `Error: No such command 'explore'`. Class-3 failure (silent until invoked). Rebuilt 2026-05-08 to point at native; previously called `python -m src.cli explore` (deleted). |
| `skills/aethyme/aethyme-load-context.sh` | `.claude/hooks/aethyme-load-context.sh` (executable, wired via `.claude/settings.local.json`) | Reads `AGENTS.md` + `CLAUDE.md` from `$CLAUDE_PROJECT_DIR`; emits SessionStart hook JSON. **Invokes two Python entry points** (registry corrected 2026-07-17 — the previous "does NOT invoke any entry point" claim predated the telemetry call): (1) `$AETHYME_ROOT/.venv/bin/python -m src.cli repo record-wrapper-invocation` (best-effort, `\|\| true`, relies on the editable install making `src.cli` importable from any cwd); (2) bare `python3` heredoc for JSON escaping of the emitted envelope. | (1) failing is silent by design — telemetry rows go missing with no visible error (fire-and-forget blind spot, see python-retirement-plan Phase 3). (2) failing (no `python3` on PATH) kills context injection entirely. Both flip to native `aethyme` invocations during retirement Phases 2–3. |

### Deployment plumbing

| Source | Deploys what | Notes |
|---|---|---|
| `src/enhance.py:TARGETS` + generated root render | fully generated `AGENTS.md`, `CLAUDE.md`, `.claude/skills/aethyme/SKILL.md`, `.codex/skills/aethyme/SKILL.md`, `.claude/skills/aethyme/references/*.md`, `.codex/skills/aethyme/references/*.md`, `.claude/hooks/aethyme-load-context.sh`, `.claude/settings.local.json` (merge-aware) | Single canonical deploy pipeline for in-repo Aethyme discoverability. Substitutes `{{AETHYME_ROOT}}`; repo-specific root customization comes from `.aethyme/overrides/agents.json`; direct edits to `AGENTS.md` / `CLAUDE.md` are unsupported and flagged by `enhance verify`. |
| `src/indexing/skills.py:deploy_skills` | `.codex/skills/<name>/*` for each runtime skill in `skills/` | Different from `enhance.py` — used by `eval/repos.py` during eval prep. As of 2026-05-08 substitutes `{{AETHYME_ROOT}}` in `.md`, `.sh`, AND the `aethyme-explore` wrapper (no extension). |

### Shell scripts in `packages/aethyme/scripts/`

| Script | Invokes | Failure mode |
|---|---|---|
| `scripts/eval/setup-playground.sh` | `python -m src.cli enhance deploy/verify` | Setup fails if `enhance` Click group is renamed/removed. |
| `scripts/eval/setup-playground.sh` | `$AETHYME_ROOT/rust/target/release/aethyme-graph-index` followed by `$AETHYME_ROOT/rust/target/release/aethyme-engine-cli index --repo .` | Fresh Playground setup fails if either binary path changes, if fragments are not produced before engine indexing, or if `graph_store.redb` stops being materialized from fragments. |
| `scripts/eval/setup-playground.sh` | Writes local `.git/info/exclude` rules for `.aethyme/`, `.chau7/`, `.claude/`, `.codex/`, `AGENTS.md`, and `CLAUDE.md`; marks tracked generated artifacts `skip-worktree` if the source repo was already enhanced. | Playground agents can treat generated scaffolding as benchmark source if these local excludes drift or are omitted. |
| `scripts/eval/setup-playground.sh` | Asserts `<target>/.aethyme/graph/` and `<target>/.aethyme/graph_store.redb` exist after graph-index + engine-index. | False failure if the fragment layout or redb graph-store path changes. |
| `scripts/eval/verify-playground.sh` | `$AETHYME_ROOT/rust/target/release/aethyme-engine-cli`; checks deployed root guidance, skill, `.aethyme/graph/`, `.aethyme/graph_store.redb`, and local generated-artifact excludes. | False failure if binary path, fragment layout, redb graph-store path, generated root wording, or ignore contract changes. |
| `scripts/eval/verify-playground.sh` | Greps deployed root files, short SKILL.md, and references for command names (`src.cli intents`, native `aethyme explore`, `analyze dead-code`, `facts function-usage`) and rejects executable `src.cli explore` guidance. | False positive/negative on health check if templates change wording but verify-playground doesn't update its grep patterns. **Updated 2026-07-27 to check generated `AGENTS.md`/`CLAUDE.md`, short-skill links, and reference files.** |
| `scripts/docs/generate-docs.sh:38` | Reads `src/cli.py` to extract command help | Doc generation fails if `cli.py` moved/renamed. |
| `scripts/migrate.sh` (REMOVED 2026-07-13) | Deleted with migrations/, alembic, and the Postgres deps in the final cloud-lineage sweep. | — |
| `scripts/start-api.sh` (REMOVED 2026-07-13) | Deleted with the Gen-0 PostgreSQL lineage (`src/api`, `src/graph`, tenant CLI commands `index/stats/ego/impact/search`). Any external caller of those entry points was already dead or must migrate to the engine CLI (`query`/`graph` commands). | — |

### Python engine adapter and eval warm phases

| Source | Invokes / assumes | Failure mode |
|---|---|---|
| `src/indexing/engine.py` | Resolves/builds `rust/target/{debug,release}/aethyme-engine-cli`; no longer invokes ANY engine subcommand (the analyze group flipped 2026-07-28, completing Phase 1); the module is a 72-line ensure-built bootstrap used by test fixtures, `pack`, `context`, `facts-*`, `analyze-dead-code`, `warm`, `workspace-inspect`, and `workspace-blast-radius`. (`symbol`, `symbol-batch`, `deps`, `impact`, the `graph-*` family, and the `task-*`/`pack`/`context` family remain supported redb surfaces but no longer have Python wrappers — the `query`, `graph`, and `task` groups went native in the router via `aethyme_engine::{query_cli,graph_cli,task_cli}`, retirement Phase 1, 2026-07-28.) It does **not** pass `--from-fragments` or `--no-fragments`, so it consumes the engine CLI default build mode. | Python CLI/API helpers silently inherit changed engine CLI defaults. If a subcommand is renamed or a global build flag becomes required, Python callers fail at runtime. |
| `src/indexing/engine.py:CACHE_ROOT` | Caches command output JSON under `AETHYME_CACHE_DIR` or `/tmp/aethyme-cache`, keyed by repo snapshot and engine identity. This is **not** `.aethyme/cache` and is separate from the removed Rust `map_cache`. | Removing Rust `map_cache` does not clear this Python output cache. If command semantics change without engine identity changing, stale output can survive until repo or binary mtime changes. |
| `src/cli.py:repo clear-cache` | Calls `clear_repository_cache(repo_path)` for the Python output cache. | If cache layout changes, user-facing cache clearing may leave stale output behind. |
| ~~`evals/tools/*.toml`, `src/eval/orchestrator.py`, `tests/local/test_eval_warm_phase.py`~~ (REMOVED 2026-07-13) | The eval harness, its tool manifests, and the eval warm-phase contract were removed with the evaluation stack (knowledge preserved in `docs/architecture/eval-mining-notes.md`; sources at git ref `16cfa5e`). The **engine daemon commands themselves** (`aethyme-engine-cli daemon status/start`, `engine-daemon.log`, the `listening on` marker) remain live engine surface — they just no longer have an in-repo eval consumer. | Out-of-repo eval scripts that invoked `aethyme eval ...` or `aethyme methodology ...` now fail with unknown command; rebuild from the mining notes if needed. |

### Single `aethyme` entrypoint (#31, 2026-07-14)

| Source | Invokes / assumes | Failure mode |
|---|---|---|
| `pyproject.toml [project.scripts]` (REMOVED 2026-07-14) | The pip console script `aethyme = "src.cli:main"` was removed — `pip install -e .` no longer installs an `aethyme` executable that shadows the Rust router. `python -m src.cli <cmd>` is the only Python-side invocation. | Out-of-repo scripts calling the *pip* `aethyme` from inside an activated venv now resolve to the Rust router (PATH) or nothing; migrate them to `python -m src.cli`. In-repo audit 2026-07-14 found zero such callers. |
| Rust router `aethyme` (installed via `cargo install --path rust/crates/aethyme-engine`) | Delegated Python commands resolve the package root via: `$AETHYME_ROOT` env → pointer file `$XDG_CONFIG_HOME/aethyme/root` (default `~/.config/aethyme/root`, managed by `aethyme root set/show`) → upward walk from cwd. Explore auto-start spawns the **sibling** `aethyme-engine-cli` (same install dir) as the daemon-serve process. | If the pointer file goes stale (checkout moved), delegated commands fail with guidance listing all three resolution options. If `aethyme-engine-cli` is not co-installed, explore auto-start falls back to PATH lookup and fails with a spawn error. |
| Generated broker protocol (`src/enhance.py:_render_broker_protocol`, deployed into repo-root `AGENTS.md`/`CLAUDE.md` of broker-configured repos; same convention hand-condensed in this repo's root `CLAUDE.md`/`AGENTS.md`) | Bare `aethyme` from PATH (`broker status/adopt/submit/close`). Since 2026-07-17 the repo-local `rust/target/release/aethyme` fallback path is no longer rendered — `cargo install --path .../aethyme-engine` is the only documented install, verifiable via `aethyme --version`. | Agents in enhanced repos run broker commands that fail with "command not found" if the router binary is renamed or the cargo-install story breaks. Re-run `enhance deploy` after protocol wording changes; text assertions live in `tests/local/test_enhance.py`. |
| Stable broker JSON command contracts (`docs/json-contracts.md`) | `aethyme broker status --json`, `aethyme broker integration status --json`, `aethyme broker events --json`, `aethyme broker metrics --json`, and `aethyme broker submit --json`. | Scripts and dashboards consuming broker state can break if these commands, field names, or enum strings are renamed/removed without a versioned contract change. |
| `.github/workflows/release.yml` (tag `v*` push) | `cargo build --release --locked -p aethyme-engine --bin aethyme --bin aethyme-engine-cli`, then `aethyme --version`, packaged per-target and attached to the GitHub release. | Renaming either `[[bin]]` target in `aethyme-engine/Cargo.toml`, removing `--version`, or letting `Cargo.lock` go stale breaks tag releases (CI-only; silent until the next tag push). |

### Installed git hooks (`aethyme broker hooks install`, 2026-07-17)

| Source | Invokes | Failure mode |
|---|---|---|
| `<git-common-dir>/hooks/pre-commit` and `post-commit` marker blocks (`# >>> aethyme hooks >>>`), written by `aethyme broker hooks install` into **every repo where an operator opted in** — canonical template: `rust/crates/aethyme-broker/src/hooks.rs:hook_block` | `"<abs-path-to-aethyme-captured-at-install>" broker hooks pre-commit` (blocking, `\|\| exit $?`) and `... broker hooks post-commit` (`\|\| true`) | **Renaming/removing the `broker hooks pre-commit` subcommand blocks ALL commits** in every repo with hooks installed: the shim treats any non-zero exit — including "unknown subcommand" — as a failed gate. `post-commit` degrades silently. A moved/deleted *binary* is handled gracefully by the shims (warn-and-pass / silent skip). Escape hatches: `git commit --no-verify`, `aethyme broker hooks uninstall`, or deleting the marker block by hand. |

### Rust `aethyme` router ↔ Python daemon (REMOVED 2026-07-13, issue #29)

| Source | Invokes / assumes | Failure mode |
|---|---|---|
| ~~`rust/crates/aethyme-engine/src/bin/aethyme.rs` ↔ `src/daemon.py`~~ | Both sides removed in the same commit (the coordinated change this row demanded). The router's repo-daemon subcommand, Python-daemon socket fallback, and socket-path logic are gone. `src/daemon.py` and its `cli.py` registration are deleted. `aethyme explore` is a native-engine-owned surface; delegated non-explore commands may still fall through to `python -m src.cli`, but they must not depend on the deleted Python daemon. Since 2026-07-14 (#31) explore runs **in-process** in the router via `aethyme_engine::explore_cli`, auto-starting the engine daemon (spawning the sibling `aethyme-engine-cli daemon serve`). | Any out-of-repo script calling `aethyme daemon start/stop/status` (repo-socket flavor, `aethyme.sock` / `aethyme-socket.path`) now falls through to the Python CLI, which reports an unknown command. The **engine** daemon (`aethyme-engine-cli daemon ...`, `engine-<hash>.sock`, eval warm phase) is a separate live contract and is unaffected — see the engine-daemon rows above. |

| Workflow | Calls | Failure mode |
|---|---|---|
| `.github/workflows/aethyme-local-tests.yml` | Runs `pytest -q tests/local`; strict-engine lane builds `cargo build --manifest-path rust/Cargo.toml --bin aethyme-engine-cli` before running the same tests. No fragment flags or redb/cache paths are passed by the workflow. | Local tests can fail if the engine binary target moves or if Python engine-adapter defaults change. The workflow itself does not consume `map_cache`, `parse_store.redb`, `.aethyme/cache`, or `.aethyme/graph_store.redb`. |
| `.github/workflows/oss-ci.yml` | Runs `pytest -q tests/local` and `cargo test --workspace`. No direct Aethyme CLI, fragment flag, or redb/cache path references. | Indirect failures surface through tests; no workflow-level path contract for 4.7 storage deletion. |
| `.github/workflows/cross-process-contract.yml` | Runs `python scripts/check-cross-process-contract.py --base ... --pr-body ...`; the script treats this document as the source of truth for protected entry-point strings. | PR contract check becomes stale if this registry misses a consumer or if the PR-template contract language changes without updating the script. |
| `.github/workflows/aethyme-gates.yml` | Creates `packages/aethyme/.venv` (`pip install -e ".[dev]"`), builds the router binary (`cargo build --release --manifest-path packages/aethyme/rust/Cargo.toml --bin aethyme`), then runs `aethyme broker gates run --all` from the repo root — the same `.aethyme/gates.toml` + runner the broker uses to verify submissions (single definition of "verified"). Emits no new event kinds (only the pre-existing `gate.*` events from the shared runner). | Breaks if the `broker gates run --all` subcommand, the `aethyme` bin target, `.aethyme/gates.toml`, or the `$MAIN/packages/aethyme/.venv` layout assumed by gate commands is renamed/moved. Convergence lane: the pre-existing test workflows above stay until this lane proves equivalent; deletion is a separate decision. |
| `packages/aethyme/.github/workflows/*` (REMOVED 2026-07-09) | The package-level workflow set (`ci.yml`, `evals.yml`, `performance.yml`, `cd.yml`, `aethyme-example.yml`) and the `aethyme-scorecard` action were deleted in the Phase 0 truth cleanup. GitHub never executes workflows outside the repo-root `.github/workflows/`, `evals.yml`/`ci.yml` referenced test paths (`tests/evals/`, `tests/integration/`) that no longer exist, and the scorecard action ran `pip install aethyme-cli` — a PyPI name this repo does not publish. | Any external repository referencing `uses: .../packages/aethyme/.github/actions/aethyme-scorecard@main` was already broken by the wrong pip package name; such callers must pin an old commit or migrate to invoking `aethyme ai-ready` directly. |

### Externally deployed runtime files (under `~/Downloads/Repositories/Playground/`)

These are files in *separate repos* (the playground clones), put there
by `deploy_skills` / `enhance.deploy`. For benchmark clones, static skill
deployment remains the compatibility path; for normal repositories,
`enhance.deploy` is the primary path because it also writes generated
onboarding artifacts.

| Path (under each `<Playground>/<repo> - Aethyme/`) | Source template | Last verified |
|---|---|---|
| `.codex/skills/aethyme/SKILL.md` | `skills/aethyme/SKILL.md` | 2026-07-27 (short-skill redeploy) |
| `.codex/skills/aethyme/references/*.md` | `skills/aethyme/references/*.md` | 2026-07-27 (short-skill redeploy) |
| `.codex/skills/aethyme/aethyme-explore` | `skills/aethyme/aethyme-explore` | 2026-05-08 (post-redeploy) |
| `AGENTS.md`, `CLAUDE.md` | `skills/aethyme/AGENTS.md` | 2026-07-27 |

### Phase 4+ graph indexer and redb graph store

These entry points belong to the Phase 1–4 graph rewrite (new
`aethyme-graph-schema`, `aethyme-graph-storage`, `aethyme-graph-indexer`
crates) and the Phase-3 redb graph store. The fragment path is now
required by the engine: the legacy pass pipeline was deleted in 4.7.12.
The durable graph contract is `.aethyme/graph/`; it is the committed
source of truth. The redb graph store at `.aethyme/graph_store.redb`
is only a derived local query artifact written by
`aethyme-engine-cli index`.

| Entry point | Source | Wire shape that becomes a contract |
|---|---|---|
| `aethyme-graph-index` binary | `crates/aethyme-graph-indexer/src/bin/aethyme-graph-index.rs` | `--repo-root`, `--repo-name`, `--engine-version`, `--skip-bootstrap`, `--max-file-size`, `--extra-ignore`, `--json` argv surface; text/JSON summary stdout; nonzero exit on failure. Eval harnesses, CI gates, and downstream scripts depend on this shape. |
| Per-file binary fragments at `<repo>/.aethyme/graph/<source>.bin` | Produced by `aethyme-graph-storage::write_fragment` | Bincode 1 of `Fragment { file_path, schema_version=1, nodes, edges }`. Tagged-enum `EdgeAttributes` uses serde's *externally tagged* form (Phase 2.1 decision); changing it is a forever-format break. |
| Per-module NDJSON shards at `<repo>/.aethyme/graph/_index/<module>.ndjson` | Produced by `aethyme-graph-storage::write_index_shard` | One `SymbolRecord` per line: `{module, symbol, kind, node_id, file}`. Sorted canonically. `merge=union` git attribute relies on the line-based form. |
| `<repo>/.aethyme/engine-version` | Produced by `aethyme-graph-storage::bootstrap_repo` | Plain text, single line, no padding. CI's parser-version-drift check reads this; downgrading or empty-on-trim is an error. |
| `<repo>/.aethyme/graph/.gitattributes` | Constant `aethyme_graph_storage::GITATTRIBUTES_CONTENT` | Two rules: `**/*.bin linguist-generated=true binary` and `_index/**/*.ndjson linguist-generated=true merge=union`. Git itself is the cross-process consumer. |
| `<repo>/.aethyme/graph_store.redb` | Written by `aethyme-engine-cli index --repo <repo>` through `store::redb::graph_store::GraphStore::reset/open`; read by query-only CLI commands through `GraphStore::open_read_only` / redb `ReadOnlyDatabase`. `index --compact` is an opt-in post-index experiment. `index --disposable-fast` writes `<repo>/.aethyme/graph_store.redb.indexing` first, then publishes it over `graph_store.redb` after the final durable metadata commit. | Playground setup and verification assert this file exists. Do not rename or relocate without updating both scripts and docs. redb read-only handles do not replace the single-writer contract; run query-only commands after the index writer has released the store. Keep compaction default-off unless MediaWiki measurements show a real persistent size reduction without meaningful latency cost. Keep disposable-fast default-off until interruption/recovery behavior has more soak time; no external consumer should depend on the `.indexing` staging file. |
| `aethyme-engine-cli --from-fragments` | Compatibility spelling in `src/bin/aethyme-engine-cli.rs`; it now selects the same fragments-only build surface as the default. | Existing diagnostics/tests may keep passing it, but it no longer bypasses or proves a separate fallback. |
| `aethyme-engine-cli --no-fragments` | Removed rollback flag retained as a hard error in `src/bin/aethyme-engine-cli.rs`; `daemon.rs` also rejects non-fragment builds. | No in-repo manifest or workflow passed it in the 4.7.9 audit. Out-of-repo callers must remove the flag and ensure `<repo>/.aethyme/graph/` exists. |

### Redb graph-store contract

`<repo>/.aethyme/graph_store.redb` is a stable local artifact path. It is
not a committed graph format, not the source of truth, and must remain
derived from committed fragments under `<repo>/.aethyme/graph/`.

Supported redb surfaces:

| Surface | Access | Contract |
|---|---|---|
| `index` | Writer | `aethyme-engine-cli index --repo <repo>` rebuilds only the derived redb store from `.aethyme/graph/` fragments. It must not modify fragments. |
| `query-areas` | Read-only | Reads area rows through `GraphStore::open_read_only` / redb `ReadOnlyDatabase`. |
| `query-overview` | Read-only | Reads repo metadata, depth-1 areas, entrypoints, and risks through the redb store. |
| `symbol` | Read-only | Reads bounded V2 function/class symbol matches from redb through exact-name, case-insensitive, prefix, component, path-component, area, and basename signals. It does not build `RepositoryMap`. |
| `symbol-batch` | Read-only | Runs the same redb-backed V2 symbol matcher for multiple queries. It does not build `RepositoryMap`. |
| `graph-node` | Read-only | Renders one node through redb display projections and exact target resolution. It preserves the existing JSON shape and does not build `RepositoryMap`. |
| `graph-children` / `graph-parents` | Read-only | Render structural relations through redb relation views. They preserve the existing JSON shape and do not build `RepositoryMap`. |
| `graph-callers` / `graph-callees` | Read-only | Render call relations through redb relation views. They preserve the existing JSON shape and do not build `RepositoryMap`. |
| `graph-docs` / `graph-configs` | Read-only | Render document/config relations through redb relation views. They preserve the existing JSON shape and do not build `RepositoryMap`. |
| `graph-expand` | Read-only | Composes the redb-backed node, relation, and risk views into the existing compact expand JSON shape. It preserves the existing bounds and does not build `RepositoryMap`. |
| `graph-overview` | Read-only | Renders the existing graph overview JSON shape from redb overview/navigation rows. It preserves the existing JSON shape and does not build `RepositoryMap`. |
| `task-expand` | Read-only | Composes redb-backed callers/callees, docs/configs, and risk views into the existing compact task expansion JSON shape. It preserves the existing JSON shape and does not build `RepositoryMap`. |
| `task-anchors` | Read-only | Resolves task anchors from redb overview rows, path indexes, config/doc rows, and bounded symbol candidates. Ranking policy remains in `graph::anchors`; the command preserves the existing JSON shape and does not build `RepositoryMap`. |
| `task-scope` | Read-only | Builds task scope from redb-backed anchors, path-prefix lookups, symbol rows, area membership, and risk lookup. It preserves the existing JSON shape and does not build `RepositoryMap`. |
| `task-next` | Read-only | Builds task navigation order from redb-backed anchors, relation views, semantic config/doc path resolution, and bounded overview slices. It preserves the existing JSON shape and does not build `RepositoryMap`. |
| `task-localize` | Read-only | Composes redb-backed `task-anchors`, `task-scope`, and `task-next` outputs. `--profile` reports redb open / task parse / anchors / scope / next / JSON stages instead of `RepositoryMap` build time. |
| `pack` / `task-pack` | Read-only + source snippets | Selects context-pack inputs from redb anchors, scope, relation, docs/config, risk, symbol, and path rows, then reads source files only to supply snippets. It does not build `RepositoryMap` in production. |
| `context` / `task-context` | Read-only + source content | Uses the same redb-selected context-pack inputs as `pack`, then reads source text for bounded content. It does not build `RepositoryMap` in production. |
| `explain` / `task-explain` | Read-only + source snippets | Renders the redb context-pack summary as text. It does not build `RepositoryMap` in production. |
| `activate` / `activate-from` / `impact` | Read-only | Expands activation and impact frontiers through redb anchors, adjacency, relations, docs/configs, area, and risk rows. It does not build `RepositoryMap` in production. |
| `explore` non-usage-boundary intents | Read-only | Native `task_localization_query`, `behavior_localization_query`, and auto-selected explore flows read graph/navigation data from redb and report redb store freshness in observability. They do not build `RepositoryMap` in production. |
| `explore --intent usage_boundary_query` | Hybrid redb + source text | Uses the usage-boundary analyzer contract below through the shared explore CLI. It does not build `RepositoryMap` and scans source/docs/config text for evidence. |
| `analyze-usage-boundary` | Hybrid redb + source text | Reads public PHP symbol seeds and candidate source/docs/config files from redb, then scans source/docs/config text for evidence. This is the accepted V2 contract because evidence strings are freshness-sensitive; a fully redb-native variant would need persisted evidence rows plus freshness/invalidation rules. It does not build `RepositoryMap` and fails cleanly when the store is missing. |
| `deps` | Read-only | Reads outgoing file adjacency from the redb store. |
| `importers` | Read-only | Reads incoming file adjacency from the redb store. |
| `callers` | Hybrid grep + graph | Greps for the requested symbol, then uses redb adjacency to expand candidate files. It is not a pure redb symbol-query contract in V1. |

Current storage coverage and limitations:

- Schema version `5` means the `index` writer populates repositories,
  directories, files, areas, functions, classes, docs, configs,
  unresolved/import placeholders, risks, `functions_by_path`, and
  `symbol_by_name` / `symbol_by_component` /
  `symbol_by_path_component`.
- The `index` writer persists the graph edge set without skipping edges for
  missing unresolved/import endpoint rows. Placeholder endpoints are stored as
  typed unresolved rows before adjacency is written.
- Task anchors, task scope, task next, task-localize, task-expand, graph
  overview, context-pack assembly, activation, impact, non-usage-boundary
  explore flows, and usage-boundary seed discovery are served from read-only
  redb rows. Usage-boundary still scans source/docs/config text for evidence
  after redb discovers public symbols and candidate files.

Remaining V2 target contract:

- No ownership change: `.aethyme/graph/` fragments remain the durable source
  of truth, while `.aethyme/graph_store.redb` remains derived, local, and
  rebuildable.
- The V2 writer must persist any remaining graph-navigation node kinds:
  separately represented methods if they stop being represented as functions,
  modules if they are introduced as separate containers, and any future
  container rows required for prefix or parent/child navigation.
- The V2 writer must persist the full graph-navigation edge set for every
  typed node kind it claims to support. It must retain incoming and outgoing
  adjacency for every persisted edge kind.
- The V2 read contract now covers typed node lookup, batch node lookup,
  display projections, area/risk/doc/config lookup, relation views,
  symbol matching with exact/prefix/component/path/area signals,
  path-prefix lookup, incoming/outgoing adjacency, task anchor candidate
  queries, usage-boundary seed discovery, and bounded overview/navigation
  slices.
- A CLI surface is not redb-backed merely because V2 tables exist. It is
  redb-backed only after the implementation reads through
  `GraphStore::open_read_only` / `ReadOnlyGraphStore` and has parity coverage
  against the current `RepositoryMap` output.

File-format policy:

- The engine currently builds against redb 4.x. The redb file format is
  owned by redb, not by Aethyme, so Aethyme does not promise in-place
  migration for old `graph_store.redb` files.
- If redb reports `UpgradeRequired(found)`, Aethyme treats the file as
  disposable and incompatible with the current engine. The operator fix is
  to regenerate it from fragments with `aethyme-engine-cli index --repo
  <repo>`.
- Query-only commands report the incompatibility and stop. They must not
  delete or regenerate the store because they are read-only consumers.

Regeneration behavior:

- `aethyme-engine-cli index --repo <repo>` is the normal writer. It detects
  old redb file formats, prints an operator message, deletes/recreates
  only `.aethyme/graph_store.redb`, and rebuilds it from
  `.aethyme/graph/`. It must not modify `.aethyme/graph/` fragments.
- `index --compact` is an opt-in post-index experiment on the same final
  store path. It is not enabled by default.
- `index --disposable-fast` is an opt-in staged writer. It writes
  `.aethyme/graph_store.redb.indexing` first, uses relaxed durability for
  bulk graph rows, performs a final durable metadata write, then publishes
  the staged file over `graph_store.redb`. The `.indexing` path is private
  and must not become a cross-process dependency.
- Normal `index` also removes stale `.indexing` files before rebuilding the
  public store, so a previously interrupted staged build does not affect
  the default writer.

Reader/writer split:

| Store access | Commands / callers | Required handle |
|---|---|---|
| Write/rebuild final store | `aethyme-engine-cli index --repo <repo>`; indirectly `scripts/eval/setup-playground.sh` after `aethyme-graph-index` writes fragments | Writable `GraphStore::reset/open` |
| Write staged store then publish | `aethyme-engine-cli index --disposable-fast --repo <repo>` | Writable `GraphStore::reset_staging`, then `publish_staging` after close |
| Optional post-write compaction | `aethyme-engine-cli index --compact --repo <repo>` | Writable `GraphStore::compact` after all write transactions commit |
| Read areas / overview | `query-areas`, `query-overview` | `GraphStore::open_read_only` / redb `ReadOnlyDatabase` |
| Read symbols | `symbol`, `symbol-batch` | `GraphStore::open_read_only` / redb `ReadOnlyDatabase` |
| Read task navigation | `task-anchors`, `task-scope`, `task-next`, `task-localize`, `task-expand` | `GraphStore::open_read_only` / redb `ReadOnlyDatabase` |
| Read adjacency | `importers`, `deps` | `GraphStore::open_read_only` / redb `ReadOnlyDatabase` |
| Hybrid usage-boundary | `analyze-usage-boundary`, `explore --intent usage_boundary_query` | `GraphStore::open_read_only` / redb `ReadOnlyDatabase` for seeds, source/docs/config text for fresh evidence |
| Hybrid grep + adjacency | `callers` | Grep first, then `GraphStore::open_read_only` / redb `ReadOnlyDatabase` for candidate expansion |
| Assert artifact exists and is locally ignored in playgrounds | `scripts/eval/setup-playground.sh`, `scripts/eval/verify-playground.sh`, `docs/guides/playground-setup.md` | Filesystem existence check plus `.git/info/exclude` visibility check in playground Aethyme clones |

Read-only open policy:

- Query-only CLI commands must use `GraphStore::open_read_only`. This is
  required, not optional: read-only commands must not create `.aethyme/`,
  initialize empty stores, repair incompatible files, or take a writable
  database handle.
- Writable `GraphStore::open/reset` is only for indexing, staging,
  schema initialization, tests that intentionally mutate the store, and
  future writer-only maintenance commands.
- Read-only opens reduce accidental writer contention, but they do not
  override redb's single-writer/file-lock behavior. Operators should run
  query-only commands after the index writer has released the store.

### Phase 4.7.9 redb/cache deletion audit

Audit command run on 2026-05-28:

```bash
rg -n -- "map_cache|AETHYME_MAP_CACHE_MAX_MB|parse_store\.redb|ParseStore|\.aethyme/cache|graph_store\.redb|--from-fragments|--no-fragments|build_from_fragments|build_with_fragment_preference" packages/aethyme
rg -n -- "aethyme-engine-cli|target/release/aethyme-engine-cli|target/debug/aethyme-engine-cli|AETHYME_ENGINE|ENGINE=|engine_bin|daemon start|daemon serve|build-profile" packages/aethyme
rg -n -- "AETHYME_CACHE_DIR|CACHE_ROOT|clear_repository_cache|_cached_text|_run_binary_command\(" packages/aethyme/src packages/aethyme/tests packages/aethyme/docs
rg -n -- "cache_dir\(|CACHE_SUBDIR|\.aethyme/cache|AETHYME_MAP_CACHE_MAX_MB" packages/aethyme/rust packages/aethyme/src packages/aethyme/scripts packages/aethyme/docs packages/aethyme/tests
```

Findings:

| Surface | Confirmed consumers | 4.7 deletion implication |
|---|---|---|
| Rust `map_cache.rs` and `AETHYME_MAP_CACHE_MAX_MB` | Rust engine internals only: before 4.7.10, `map.rs` loaded/saved the cache and `lib.rs` exported the module. No Python, shell, eval manifest, or skill consumer found. | Deleted in 4.7.10. This does not affect Python's separate `/tmp/aethyme-cache` output cache. |
| `<repo>/.aethyme/cache/map-*.bin` | Deleted Rust `map_cache.rs` only. `aethyme-graph-storage::cache_dir` also reserves `.aethyme/cache` as a layout helper for future local mirrors, but no external script consumes files in that directory. | 4.7.10 may orphan old `map-*.bin` files. Do not delete or rename the generic `cache_dir` layout helper; it belongs to graph-storage layout, not the removed map-cache implementation. |
| `ParseStore` and `<repo>/.aethyme/parse_store.redb` | Rust engine internals only before 4.7.11. No Python, shell, eval manifest, or skill consumer found. | Deleted in 4.7.11; the legacy passes that used it were deleted in 4.7.12. The on-disk `parse_store.redb` is now an orphan local cache file. |
| `<repo>/.aethyme/graph_store.redb` | `scripts/eval/setup-playground.sh`, `scripts/eval/verify-playground.sh`, `docs/guides/playground-setup.md`, `docs/architecture/graph-schema.md`, and `store/redb/graph_store.rs`. | Keep stable. It is the externally asserted redb graph-store artifact and is separate from deleted `ParseStore`. |
| Engine CLI fragment flags | `aethyme-engine-cli.rs` accepts `--from-fragments` as compatibility spelling and rejects `--no-fragments`. No external manifest currently passes either flag. | 4.7.12 removed the rollback path. Keep the hard-error message stable enough for operators to diagnose stale scripts. |
| Engine daemon redb query server | `aethyme explore` auto-start and any external users of `aethyme-engine-cli daemon start/status/serve`. Removed eval warm-phase files are historical only. | Daemon start/status names, `engine-daemon.log`, and `listening on` are cross-process contracts. The daemon no longer builds or warms `RepositoryMap`; it opens `.aethyme/graph_store.redb` read-only before listening and serves migrated task/symbol/caller RPCs through redb. |

Outstanding risks after 4.7.12:

- The audit only covers in-repo consumers under `packages/aethyme/`; already-deployed playground repos can still contain stale generated skills or missing local excludes. Run `scripts/eval/verify-playground.sh` against active playground targets after any template or playground-hygiene change.
- Python output caches under `AETHYME_CACHE_DIR` are independent of the removed Rust `map_cache`; deletion of `map_cache.rs` does not clear cached JSON command output.
- Any out-of-repo scripts that read orphaned `.aethyme/cache/map-*.bin` or `.aethyme/parse_store.redb` files directly, or pass `--no-fragments`, are unsupported and were not found by this repo-local audit.
- The Rust engine now builds against redb 4.x. Existing `.aethyme/graph_store.redb` files created by the redb 2.x engine should be treated as disposable local materializations and regenerated from `.aethyme/graph/` fragments with `aethyme-engine-cli index --repo <repo>`; the index command reports old redb file formats before deleting/recreating the graph store. Do not rely on an in-place redb file-format migration for deploys.
- Query-only CLI commands now open `.aethyme/graph_store.redb` with redb `ReadOnlyDatabase`. This prevents inspectors from taking a writable database handle, but redb still rejects read-only opens while a writable `Database` is open on platforms with file locks. Future daemon work needs an explicit ownership model if it keeps the writable graph store open.
- The 2026-05-29 post-index compaction experiment does not justify enabling compaction by default. `index --compact` added 614 ms on Mockup and 283 ms on MediaWiki, produced no persistent Mockup reduction, grew MediaWiki from 33,689,600 to 44,265,472 bytes, and moved MediaWiki `query-overview` median latency from 10.0 ms to 11.3 ms.
- The 2026-05-29 disposable-fast experiment is opt-in. It uses redb `Durability::None` for bulk node/edge/risk commits, keeps the final metadata write at immediate durability, and publishes from `.aethyme/graph_store.redb.indexing` only after the staged store is complete. Initial profile runs improved redb write stages on Mockup (`redb_edge_writes` 925 ms to 528 ms, `redb_commit` 28 ms to 5 ms, plus 143 ms metadata and 124 ms publish) and MediaWiki (`redb_edge_writes` 292 ms to 180 ms, `redb_commit` 30 ms to 12 ms, plus 60 ms metadata and 31 ms publish). Keep it guarded until interrupted-process recovery has more field evidence.

## Migration checklist

When deleting or renaming a CLI entry point:

- [ ] Grep this file for the entry-point name; list affected consumers.
- [ ] For each Skill template (`SKILL.md`, etc.): update the wording.
- [ ] For each shell wrapper (`aethyme-explore`, etc.): update the
      `exec` line.
- [ ] For `verify-playground.sh`: update the grep patterns.
- [ ] For `setup-playground.sh`: update if it calls the entry point.
- [ ] Run `aethyme repo deploy-skills --force` against any active
      benchmark playground clone to flush the stale static copy.
- [ ] Run `aethyme enhance deploy --repo <path>` against any normal
      repository enhancement target to refresh generated onboarding.
- [ ] Add a row to the relevant section above documenting the change.
- [ ] If a consumer was missed (i.e. you discovered it during a
      validation eval rather than this audit): **add it to this file
      in the same commit as the fix.** That's how the registry stays
      complete.

## Failure modes (taxonomy)

When a cross-process consumer references a deleted entry point:

- **Class 1: load-time failure** — script syntax-checks fail or imports
  fail. Loud. Caught immediately.
- **Class 2: invocation failure** — script runs but `aethyme <cmd>`
  errors with `No such command`. Loud once invoked. May go undetected
  if the script's invocation path is rare.
- **Class 3: silent stale guidance** — script reads file content and
  matches/passes/fails based on string presence; updated entry point
  passes the wrong checks. The 2026-05-08 wrapper bug AND the
  `verify-playground.sh:135` grep flip-bug were both Class 3. **These
  are the dangerous ones**: nothing visibly fails, but the system's
  health checks are now lying.

Class-3 failures are why this registry exists. Static analysis can't
help; only manual audit of every consumer can.
