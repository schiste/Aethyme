# Cross-process consumers of Aethyme entry points

Last Updated: 2026-07-15

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
| `skills/aethyme/SKILL.md` | `.claude/skills/aethyme/SKILL.md`, `.codex/skills/aethyme/SKILL.md` | Documents `aethyme explore`, `aethyme query symbol`, `aethyme graph callers/callees`, `analyze dead-code`, `facts function-usage`, `task scope/anchors`, `intents`, `task context/pack` by name | Agent reads stale guidance, runs commands that no longer exist; user sees `Error: No such command 'X'`. Caught by `verify-playground.sh` greps and `scripts/check-cross-process-contract.py` text-consumer validation. |
| `skills/aethyme/AGENTS.md` | fully generated `AGENTS.md` and `CLAUDE.md` (deployed at repo root by `enhance.py`) | Cross-product convention file with quick-start command guidance, compact repo routing from generated onboarding/status artifacts, and commit hygiene policy. Root files are Aethyme-owned generated artifacts; repo-specific human customizations come from `.aethyme/overrides/agents.json`. | Agent reads stale quick start before loading skill details. Caught by `scripts/check-cross-process-contract.py` text-consumer validation and `enhance verify` canonical-match checks. |
| `skills/aethyme/aethyme-explore` | `.codex/skills/aethyme/aethyme-explore` (executable) | `exec "{{AETHYME_ROOT}}/rust/target/release/aethyme" explore "$@"` | Wrapper produces `Error: No such command 'explore'`. Class-3 failure (silent until invoked). Rebuilt 2026-05-08 to point at native; previously called `python -m src.cli explore` (deleted). |
| `skills/aethyme/aethyme-load-context.sh` | `.claude/hooks/aethyme-load-context.sh` (executable, wired via `.claude/settings.local.json`) | Reads `AGENTS.md` + `CLAUDE.md` from `$CLAUDE_PROJECT_DIR`; emits SessionStart hook JSON. **Does NOT invoke any Aethyme entry point.** | Hook fails to inject context; agent loses the in-repo discoverability surface. |

### Deployment plumbing

| Source | Deploys what | Notes |
|---|---|---|
| `src/enhance.py:TARGETS` + generated root render | fully generated `AGENTS.md`, `CLAUDE.md`, `.claude/skills/aethyme/SKILL.md`, `.codex/skills/aethyme/SKILL.md`, `.claude/hooks/aethyme-load-context.sh`, `.claude/settings.local.json` (merge-aware) | Single canonical deploy pipeline for in-repo Aethyme discoverability. Substitutes `{{AETHYME_ROOT}}`; repo-specific root customization comes from `.aethyme/overrides/agents.json`; direct edits to `AGENTS.md` / `CLAUDE.md` are unsupported and flagged by `enhance verify`. |
| `src/indexing/skills.py:deploy_skills` | `.codex/skills/<name>/*` for each runtime skill in `skills/` | Different from `enhance.py` — used by `eval/repos.py` during eval prep. As of 2026-05-08 substitutes `{{AETHYME_ROOT}}` in `.md`, `.sh`, AND the `aethyme-explore` wrapper (no extension). |

### Shell scripts in `packages/aethyme/scripts/`

| Script | Invokes | Failure mode |
|---|---|---|
| `scripts/eval/setup-playground.sh` | `python -m src.cli enhance deploy/verify` | Setup fails if `enhance` Click group is renamed/removed. |
| `scripts/eval/setup-playground.sh` | `$AETHYME_ROOT/rust/target/release/aethyme-graph-index` followed by `$AETHYME_ROOT/rust/target/release/aethyme-engine-cli index --repo .` | Fresh Playground setup fails if either binary path changes, if fragments are not produced before engine indexing, or if `graph_store.redb` stops being materialized from fragments. |
| `scripts/eval/setup-playground.sh` | Asserts `<target>/.aethyme/graph/` and `<target>/.aethyme/graph_store.redb` exist after graph-index + engine-index. | False failure if the fragment layout or redb graph-store path changes. |
| `scripts/eval/verify-playground.sh` | `$AETHYME_ROOT/rust/target/release/aethyme-engine-cli`; checks deployed skill, `.aethyme/graph/`, and `.aethyme/graph_store.redb`. | False failure if binary path, fragment layout, or redb graph-store path changes. |
| `scripts/eval/verify-playground.sh:131-138` | Greps deployed SKILL.md for command names (`src.cli intents`, `aethyme explore`, `analyze dead-code`, `facts function-usage`) | False positive/negative on health check if SKILL.md template changes wording but verify-playground doesn't update its grep patterns. **Updated 2026-05-08 after the explore hard-delete to expect native `aethyme explore` not `src.cli explore`.** |
| `scripts/docs/generate-docs.sh:38` | Reads `src/cli.py` to extract command help | Doc generation fails if `cli.py` moved/renamed. |
| `scripts/migrate.sh` (REMOVED 2026-07-13) | Deleted with migrations/, alembic, and the Postgres deps in the final cloud-lineage sweep. | — |
| `scripts/start-api.sh` (REMOVED 2026-07-13) | Deleted with the Gen-0 PostgreSQL lineage (`src/api`, `src/graph`, tenant CLI commands `index/stats/ego/impact/search`). Any external caller of those entry points was already dead or must migrate to the engine CLI (`query`/`graph` commands). | — |

### Python engine adapter and eval warm phases

| Source | Invokes / assumes | Failure mode |
|---|---|---|
| `src/indexing/engine.py` | Resolves/builds `rust/target/{debug,release}/aethyme-engine-cli`; calls many subcommands through `_run_binary_command`, including `inspect`, `symbol`, `symbol-batch`, `graph-*`, `task-*`, `pack`, `context`, `facts-*`, `analyze-dead-code`, `warm`, `workspace-inspect`, and `workspace-blast-radius`. It does **not** pass `--from-fragments` or `--no-fragments`, so it consumes the engine CLI default build mode. | Python CLI/API helpers silently inherit changed engine CLI defaults. If a subcommand is renamed or a global build flag becomes required, Python callers fail at runtime. |
| `src/indexing/engine.py:CACHE_ROOT` | Caches command output JSON under `AETHYME_CACHE_DIR` or `/tmp/aethyme-cache`, keyed by repo snapshot and engine identity. This is **not** `.aethyme/cache` and is separate from the removed Rust `map_cache`. | Removing Rust `map_cache` does not clear this Python output cache. If command semantics change without engine identity changing, stale output can survive until repo or binary mtime changes. |
| `src/cli.py:repo clear-cache` | Calls `clear_repository_cache(repo_path)` for the Python output cache. | If cache layout changes, user-facing cache clearing may leave stale output behind. |
| ~~`evals/tools/*.toml`, `src/eval/orchestrator.py`, `tests/local/test_eval_warm_phase.py`~~ (REMOVED 2026-07-13) | The eval harness, its tool manifests, and the eval warm-phase contract were removed with the evaluation stack (knowledge preserved in `docs/architecture/eval-mining-notes.md`; sources at git ref `16cfa5e`). The **engine daemon commands themselves** (`aethyme-engine-cli daemon status/start`, `engine-daemon.log`, the `listening on` marker) remain live engine surface — they just no longer have an in-repo eval consumer. | Out-of-repo eval scripts that invoked `aethyme eval ...` or `aethyme methodology ...` now fail with unknown command; rebuild from the mining notes if needed. |

### Single `aethyme` entrypoint (#31, 2026-07-14)

| Source | Invokes / assumes | Failure mode |
|---|---|---|
| `pyproject.toml [project.scripts]` (REMOVED 2026-07-14) | The pip console script `aethyme = "src.cli:main"` was removed — `pip install -e .` no longer installs an `aethyme` executable that shadows the Rust router. `python -m src.cli <cmd>` is the only Python-side invocation. | Out-of-repo scripts calling the *pip* `aethyme` from inside an activated venv now resolve to the Rust router (PATH) or nothing; migrate them to `python -m src.cli`. In-repo audit 2026-07-14 found zero such callers. |
| Rust router `aethyme` (installed via `cargo install --path rust/crates/aethyme-engine`) | Delegated Python commands resolve the package root via: `$AETHYME_ROOT` env → pointer file `$XDG_CONFIG_HOME/aethyme/root` (default `~/.config/aethyme/root`, managed by `aethyme root set/show`) → upward walk from cwd. Explore auto-start spawns the **sibling** `aethyme-engine-cli` (same install dir) as the daemon-serve process. | If the pointer file goes stale (checkout moved), delegated commands fail with guidance listing all three resolution options. If `aethyme-engine-cli` is not co-installed, explore auto-start falls back to PATH lookup and fails with a spawn error. |

### Rust `aethyme` router ↔ Python daemon (REMOVED 2026-07-13, issue #29)

| Source | Invokes / assumes | Failure mode |
|---|---|---|
| ~~`rust/crates/aethyme-engine/src/bin/aethyme.rs` ↔ `src/daemon.py`~~ | Both sides removed in the same commit (the coordinated change this row demanded). The router's repo-daemon subcommand, Python-daemon socket fallback, and socket-path logic are gone. `src/daemon.py` and its `cli.py` registration are deleted. `aethyme explore` is a native-engine-owned surface; delegated non-explore commands may still fall through to `python -m src.cli`, but they must not depend on the deleted Python daemon. Since 2026-07-14 (#31) explore runs **in-process** in the router via `aethyme_engine::explore_cli`, auto-starting the engine daemon (spawning the sibling `aethyme-engine-cli daemon serve`). | Any out-of-repo script calling `aethyme daemon start/stop/status` (repo-socket flavor, `aethyme.sock` / `aethyme-socket.path`) now falls through to the Python CLI, which reports an unknown command. The **engine** daemon (`aethyme-engine-cli daemon ...`, `engine-<hash>.sock`, eval warm phase) is a separate live contract and is unaffected — see the engine-daemon rows above. |

| Workflow | Calls | Failure mode |
|---|---|---|
| `.github/workflows/aethyme-local-tests.yml` | Runs `pytest -q tests/local`; strict-engine lane builds `cargo build --manifest-path rust/Cargo.toml --bin aethyme-engine-cli` before running the same tests. No fragment flags or redb/cache paths are passed by the workflow. | Local tests can fail if the engine binary target moves or if Python engine-adapter defaults change. The workflow itself does not consume `map_cache`, `parse_store.redb`, `.aethyme/cache`, or `.aethyme/graph_store.redb`. |
| `.github/workflows/oss-ci.yml` | Runs `pytest -q tests/local` and `cargo test --workspace`. No direct Aethyme CLI, fragment flag, or redb/cache path references. | Indirect failures surface through tests; no workflow-level path contract for 4.7 storage deletion. |
| `.github/workflows/cross-process-contract.yml` | Runs `python scripts/check-cross-process-contract.py --base ... --pr-body ...`; the script treats this document as the source of truth for protected entry-point strings. | PR contract check becomes stale if this registry misses a consumer or if the PR-template contract language changes without updating the script. |
| `packages/aethyme/.github/workflows/*` (REMOVED 2026-07-09) | The package-level workflow set (`ci.yml`, `evals.yml`, `performance.yml`, `cd.yml`, `aethyme-example.yml`) and the `aethyme-scorecard` action were deleted in the Phase 0 truth cleanup. GitHub never executes workflows outside the repo-root `.github/workflows/`, `evals.yml`/`ci.yml` referenced test paths (`tests/evals/`, `tests/integration/`) that no longer exist, and the scorecard action ran `pip install aethyme-cli` — a PyPI name this repo does not publish. | Any external repository referencing `uses: .../packages/aethyme/.github/actions/aethyme-scorecard@main` was already broken by the wrong pip package name; such callers must pin an old commit or migrate to invoking `aethyme ai-ready` directly. |

### Externally deployed runtime files (under `~/Downloads/Repositories/Playground/`)

These are files in *separate repos* (the playground clones), put there
by `deploy_skills` / `enhance.deploy`. For benchmark clones, static skill
deployment remains the compatibility path; for normal repositories,
`enhance.deploy` is the primary path because it also writes generated
onboarding artifacts.

| Path (under each `<Playground>/<repo> - Aethyme/`) | Source template | Last verified |
|---|---|---|
| `.codex/skills/aethyme/SKILL.md` | `skills/aethyme/SKILL.md` | 2026-05-08 (post-redeploy) |
| `.codex/skills/aethyme/aethyme-explore` | `skills/aethyme/aethyme-explore` | 2026-05-08 (post-redeploy) |
| `AGENTS.md`, `CLAUDE.md` | `skills/aethyme/AGENTS.md` | 2026-05-08 |

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

Supported V1 redb surfaces:

| Surface | Access | Contract |
|---|---|---|
| `index` | Writer | `aethyme-engine-cli index --repo <repo>` rebuilds only the derived redb store from `.aethyme/graph/` fragments. It must not modify fragments. |
| `query-areas` | Read-only | Reads area rows through `GraphStore::open_read_only` / redb `ReadOnlyDatabase`. |
| `query-overview` | Read-only | Reads repo metadata, depth-1 areas, entrypoints, and risks through the redb store. |
| `deps` | Read-only | Reads outgoing file adjacency from the redb store. |
| `importers` | Read-only | Reads incoming file adjacency from the redb store. |
| `callers` | Hybrid grep + graph | Greps for the requested symbol, then uses redb adjacency to expand candidate files. It is not a pure redb symbol-query contract in V1. |

Supported V1 limitations:

- Symbol-level redb persistence is not complete. The redb schema includes
  `functions`, `classes`, `functions_by_path`, and `symbol_by_name` tables as
  schema-ready surfaces, but the current `index` writer does not fully
  populate them from `RepositoryMap`.
- The `index` writer skips edges with `fn:` or `class:` endpoints before
  writing redb adjacency. `deps`, `importers`, and the graph half of
  `callers` are file-adjacency surfaces, not general symbol-callgraph
  surfaces.
- Most graph navigation is still served by `RepositoryMap` rebuilt from
  `.aethyme/graph/` fragments. The legacy `graph-*`, task, context-pack, and
  `explore` surfaces should not be described as redb-backed until they are
  explicitly moved to `GraphStore::open_read_only`.

V2 target contract:

- No ownership change: `.aethyme/graph/` fragments remain the durable source
  of truth, while `.aethyme/graph_store.redb` remains derived, local, and
  rebuildable.
- The V2 writer must persist the full graph-navigation node set: files,
  areas, functions, classes, separately represented methods, docs/configs
  used by navigation, unresolved/import nodes when they participate in lookup
  or edges, and any derived directory/module/container nodes required for
  prefix or parent/child navigation.
- The V2 writer must persist the full graph-navigation edge set, including
  symbol-level endpoints. It must retain incoming and outgoing adjacency for
  every persisted edge kind.
- The V2 read contract must cover `node_by_id`, batch node lookup, symbol
  lookup by name/kind, path-prefix lookup, incoming/outgoing adjacency, task
  anchor candidate queries, and bounded overview/navigation slices.
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
| Read adjacency | `importers`, `deps` | `GraphStore::open_read_only` / redb `ReadOnlyDatabase` |
| Hybrid grep + adjacency | `callers` | Grep first, then `GraphStore::open_read_only` / redb `ReadOnlyDatabase` for candidate expansion |
| Assert artifact exists | `scripts/eval/setup-playground.sh`, `scripts/eval/verify-playground.sh`, `docs/guides/playground-setup.md` | Filesystem existence check only |

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
| Engine CLI fragment flags | `aethyme-engine-cli.rs` accepts `--from-fragments` as compatibility spelling and rejects `--no-fragments`; daemon builds require fragments. No external manifest currently passes either flag. | 4.7.12 removed the rollback path. Keep the hard-error message stable enough for operators to diagnose stale scripts. |
| Engine daemon warm command | `evals/tools/aethyme.toml`, `src/eval/orchestrator.py`, and `tests/local/test_eval_warm_phase.py`. | Daemon start/status names, `engine-daemon.log`, and `listening on` are cross-process contracts. Fragment-only behavior can change underneath, but command/log shape should not change in the 4.7 cutover. |

Outstanding risks after 4.7.12:

- The audit only covers in-repo consumers under `packages/aethyme/`; already-deployed playground repos can still contain stale generated skills. Run `scripts/eval/verify-playground.sh` against active playground targets after any template change.
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
