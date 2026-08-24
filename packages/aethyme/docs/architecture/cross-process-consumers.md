# Cross-process consumers of Aethyme entry points

Last Updated: 2026-08-24

When code outside the `packages/aethyme/` Rust source tree invokes an
Aethyme command, it crosses a process boundary. Static
analysis (ruff, cargo check, type checkers) does not see those
invocations. They have to be audited by hand.

This file is the canonical inventory: every shell script, hook, skill
template, deployed wrapper, and CI step that calls an Aethyme entry
point. **Before deleting or renaming any CLI entry point — Rust
binary subcommand or shell helper — grep this file for callers and
update each.**

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

### Skill template files (canonical sources, deployed by the `aethyme-enhance` crate)

| Source | Deployed to | Invokes | Failure mode if entry point removed |
|---|---|---|---|
| `skills/aethyme/SKILL.md` | `.claude/skills/aethyme/SKILL.md`, `.codex/skills/aethyme/SKILL.md` | Short auto-load card: one bounded native `aethyme explore` call, `aethyme explore-summary --from "$AETHYME_JSON"` for the compact projection (**flipped from a `"$AETHYME_ROOT/.venv/bin/python"` heredoc in the Phase 5.5 projection flip, 2026-08-01**; the `AETHYME_PY` setup line is gone), then `aethyme verify-targets` over the SAME temp file. Links detailed workflows under `references/`. | Agent reads stale guidance, runs commands that no longer exist, or bulk-loads detailed workflows. Renaming/removing `explore-summary` breaks the projection step in every enhanced repo (Class-3: silent until an agent runs it). Caught by `verify-playground.sh` greps (`explore-summary --from` present, no `.venv/bin/python` invocation), `aethyme-cli/tests/skill_templates.rs`, `aethyme-cli/tests/explore_summary_cli.rs`, and `aethyme broker check-contract` text-consumer validation. |
| `skills/aethyme/references/*.md` | `.claude/skills/aethyme/references/*.md`, `.codex/skills/aethyme/references/*.md` | Detailed optional workflows: Explore depth/intents/trust, graph/task/context commands, and dead-code/facts/analyzer commands. All spelled `aethyme ...` since the 2026-07-30 Phase 2 template flip; since the Phase 3 flip (2026-07-29) every referenced `repo` subcommand answers natively (`aethyme_engine::repo_cli` + `aethyme_enhance::repo_cli`), no Python delegation. `references/explore.md` calls `aethyme explore-summary --from` for the compact projection (**flipped from the `"$AETHYME_PY"` heredoc in the Phase 5.5 projection flip, 2026-08-01**). | Enhanced repos receive a short skill with broken reference links or stale detailed commands. Caught by `enhance verify`, `verify-playground.sh` (`explore-summary --from` present, no `.venv/bin/python` invocation in any reference), and reference deployment tests. |
| `skills/aethyme/AGENTS.md` | fully generated `AGENTS.md` and `CLAUDE.md` (deployed at repo root by `aethyme enhance deploy`, aethyme-enhance crate) | Cross-product convention file with quick-start command guidance, compact repo routing from generated onboarding/status artifacts, commit hygiene policy, and broker coordination. Broker-configured output distinguishes local verified submission from remote publication, retains the full Git/GitHub CLI surface when the resulting operation is explicitly authorized, and routes coordinated mutations through `aethyme broker git`, `aethyme broker gh`, and crash recovery through `aethyme broker operations reconcile`. The quick start runs `aethyme explore` → `aethyme explore-summary --from` → `aethyme verify-targets --from` over one temp file (**the projection step flipped from a `"$AETHYME_ROOT/.venv/bin/python"` heredoc in the Phase 5.5 projection flip, 2026-08-01**). Root files are Aethyme-owned generated artifacts; repo-specific human customizations come from `.aethyme/overrides/agents.json`. | Agent reads stale quick start before loading skill details; removing `explore-summary` or the coordinated-operation commands breaks generated guidance in every enhanced broker repo. Caught by `aethyme broker check-contract` text-consumer validation, `verify-playground.sh` root-guidance greps, `aethyme-cli/tests/playground_hygiene.rs`, `aethyme-cli/tests/enhance_cli.rs`, and `enhance verify` canonical-match checks. |
| `skills/aethyme/aethyme-explore` | `.codex/skills/aethyme/aethyme-explore` (executable) | Resolves `aethyme` from PATH once, records a best-effort wrapper invocation, then executes that same installed router with `explore "$@"`. The deployed wrapper has no source-checkout or `AETHYME_ROOT` dependency. | Wrapper reports that `aethyme` is not installed when PATH lacks the router; removing `explore` breaks every deployed wrapper. Class-3 failure (silent until invoked). Deployed-wrapper freshness is checked by `verify-playground.sh` and the installed-binary deployment tests. |
| `skills/aethyme/aethyme-load-context.sh` | `.claude/hooks/aethyme-load-context.sh` (executable, wired via `.claude/settings.local.json`) | Reads `AGENTS.md` + `CLAUDE.md` from `$CLAUDE_PROJECT_DIR`; resolves the installed `aethyme` from PATH once; records wrapper telemetry best-effort; then invokes `aethyme repo hook-envelope` over stdin to emit SessionStart JSON. It has no source-checkout fallback. | With no router on PATH the hook exits 0 emitting nothing rather than injecting malformed context. Removing `repo hook-envelope` breaks context injection. Hook end-to-end coverage lives in `enhance_cli.rs`, `skill_templates.rs`, and the no-Python product CI lane. Re-run `aethyme deploy` in already-enrolled repositories after upgrading. |

### Deployment plumbing

| Source | Deploys what | Notes |
|---|---|---|
| Router `aethyme deploy` / `aethyme deploy verify` | Broker configuration and gates, portable onboarding inputs, generated root policy, agent skills, and hook settings | Canonical mandatory repository enrollment. `deploy` composes scaffold, gate drafting, embedded enhance deployment, verification, and certification; `deploy verify` is read-only and intended for required CI. Both run entirely from the installed binary pair. |
| Router `aethyme deploy bridge`, `deploy --local-only`, and `deploy verify --local-only` | A committed inert AGENTS/CLAUDE marker check plus clone-ignored full policy, skills, broker configuration, gates, and runtime state | Staged adoption surface. Inactive clones must not probe PATH, invoke hooks, install Aethyme, or warn. Active clones load `.aethyme/local/AGENTS.md` as mandatory through `.aethyme/local/enabled`; tracked-policy collisions are refused and verification is read-only. Covered by `repository_deploy_cli`. |
| `aethyme-enhance` crate deploy pipeline (`deploy.rs:TARGETS` + `agents.rs` root render; templates embedded at build time — Phase 2 flip, 2026-07-29) | fully generated `AGENTS.md`, `CLAUDE.md`, `.claude/skills/aethyme/SKILL.md`, `.codex/skills/aethyme/SKILL.md`, `.claude/skills/aethyme/references/*.md`, `.codex/skills/aethyme/references/*.md`, `.claude/hooks/aethyme-load-context.sh`, `.claude/settings.local.json` (merge-aware) | Embedded lower-level discoverability pipeline used by `aethyme deploy`; all generated commands resolve `aethyme` from PATH and require no source checkout. Repo-specific customization comes from `.aethyme/overrides/agents.json`; direct edits to `AGENTS.md` / `CLAUDE.md` are unsupported and flagged by `enhance verify`. |
| `aethyme-enhance` crate `skills.rs` (`repo deploy-skills`, native since the Phase 3 flip 2026-07-29; the Python `src/indexing/skills.py` is deleted — its former `eval/repos.py` consumer was already removed with the eval stack) | `.codex/skills/<name>/*` for each runtime skill (embedded templates) | Compatibility path for static runtime-skill deployment. It renders the same PATH-resolved installed commands as canonical deployment; `--remove` also sweeps the legacy `eval` skill directory older tooling may have deployed. |

### Shell scripts in `packages/aethyme/scripts/`

| Script | Invokes | Failure mode |
|---|---|---|
| `scripts/eval/setup-playground.sh` | `$AETHYME_ROOT/rust/target/release/aethyme enhance deploy/verify` (native since the Phase 2 flip, 2026-07-29) | Setup fails if the router binary is missing (preflight-checked) or the `enhance deploy`/`enhance verify` subcommands are renamed/removed. |
| `scripts/eval/setup-playground.sh` | `$AETHYME_ROOT/rust/target/release/aethyme-graph-index` followed by `$AETHYME_ROOT/rust/target/release/aethyme-engine-cli index --repo .` | Fresh Playground setup fails if either binary path changes, if fragments are not produced before engine indexing, or if `graph_store.redb` stops being materialized from fragments. |
| `scripts/eval/setup-playground.sh` | Writes local `.git/info/exclude` rules for `.aethyme/`, `.chau7/`, `.claude/`, `.codex/`, `AGENTS.md`, and `CLAUDE.md`; marks tracked generated artifacts `skip-worktree` if the source repo was already enhanced. | Playground agents can treat generated scaffolding as benchmark source if these local excludes drift or are omitted. |
| `scripts/eval/setup-playground.sh` | Asserts `<target>/.aethyme/graph/` and `<target>/.aethyme/graph_store.redb` exist after graph-index + engine-index. | False failure if the fragment layout or redb graph-store path changes. |
| `scripts/eval/verify-playground.sh` | `$AETHYME_ROOT/rust/target/release/aethyme-engine-cli`; checks deployed root guidance, skill, `.aethyme/graph/`, `.aethyme/graph_store.redb`, and local generated-artifact excludes. | False failure if binary path, fragment layout, redb graph-store path, generated root wording, or ignore contract changes. |
| `scripts/eval/verify-playground.sh` | Greps deployed root files, short SKILL.md, and references for command names (native `aethyme explore`, `aethyme explore-summary --from`, `analyze dead-code`, `facts function-usage`) and rejects ANY executable `python -m src.cli` guidance (Phase 2 template flip 2026-07-30: deployed commands spell `aethyme ...`; tolerated mentions are negative-guidance lines matching 'Do not run\|not a valid command\|was removed\|retired'). Since the Phase 5.5 projection flip (2026-08-01) `check_no_venv_python` also rejects any `.venv/bin/python` invocation in deployed guidance, wrappers, and hooks — comment/provenance lines tolerated. | False positive/negative on health check if templates change wording but verify-playground doesn't update its grep patterns. **Updated 2026-07-30 in lockstep with the template spelling flip and 2026-08-01 with the Phase 5.5 projection flip (Class 3 protocol); assertions mirrored in `aethyme-cli/tests/playground_hygiene.rs`.** |
| ~~`scripts/docs/generate-docs.sh`~~ (REMOVED 2026-08-01, retirement Phase 6) | Dead generator. Its OpenAPI block read `src/api/main.py` (deleted with the Gen-0 PostgreSQL lineage 2026-07-13), its CLI block iterated `src/cli.py`'s Click tree (empty since Phase 5, deleted in Phase 6), and every file it claimed to write — `docs/openapi.json`, `docs/reference/cli-generated.md`, `docs/reference/metrics.md`, `docs/INDEX.md` — is absent from the tree, so nothing consumed its output. It was also one of the last `python3` invocations inside `packages/aethyme`. | No replacement: `docs/reference/cli.md` is hand-maintained and `aethyme --help` is the live surface. Anyone running the script got two skipped sections and a metrics doc describing a Prometheus endpoint that no longer exists. |
| ~~`scripts/eval/run_codex_eval.py`, `scripts/eval/check_regression_gate.py`~~ (MOVED 2026-08-06, retirement Phase 7) | Both moved to `packages/aethyme-eval/scripts/` — eval tooling, in the package that owns eval tooling and stays Python by operator decision, because an arm's-length acceptance check should not share the measured system's toolchain. `packages/aethyme` is 100% Rust now, so staying was not an option either. Their contract test moved with them (`packages/aethyme-eval/tests/test_codex_eval_runner_contract.py`, 41 cases). The move touched path arithmetic that encodes **cardinal rule 1**: both files derived `package_root`/`monorepo_root` by counting `parents[2]`; they now name `TOOL_PACKAGE_ROOT` (the measured `packages/aethyme`, still the default `AETHYME_EVAL_TOOL_REPO`) and `MONOREPO_ROOT` (an eval target inside it is Aethyme itself and is refused) explicitly. | Out-of-repo automation invoking `packages/aethyme/scripts/eval/run_codex_eval.py` gets "No such file"; the new paths are `packages/aethyme-eval/scripts/{run_codex_eval,check_regression_gate}.py`, run with **aethyme-eval's** interpreter (`packages/aethyme/.venv` no longer exists). If the two root constants are ever miscomputed, the eval could silently run against Aethyme itself — the 41-case contract suite covers exactly this, passing `packages/aethyme` as a repo path the contract must refuse. Docs updated in the same commit: `docs/guides/playground-setup.md`, `docs/guides/eval-protocol.md`, `docs/reference/cli.md`, `packages/aethyme-eval/README.md`. |
| ~~`scripts/verify-grammar-provenance.py`~~ (PORTED 2026-08-06, retirement Phase 7) | The tree-sitter grammar provenance verifier now lives at `rust/crates/aethyme-testkit/tests/grammar_provenance.rs` — it audits the Rust crates' own grammar assets, so it is `packages/aethyme`'s concern and ported rather than moving to aethyme-eval. Same manifest shape, file-presence, MIT-license and sha256 checks, same error strings, same two modes. **No test ever invoked the script**; both modes were manual commands, so making the development mode a `cargo test` case is a strict gain. Release mode (`--require-pinned`) is `#[ignore]`d because it is documented as expected to fail until every grammar records a pinned upstream ref. | Anyone running `python packages/aethyme/scripts/verify-grammar-provenance.py` gets "No such file". Replacements: `cargo test -p aethyme-testkit --test grammar_provenance` (development) and `... -- --ignored` (release). Callers updated in the same commit: `docs/third-party-license-audit.md`, `rust/grammars/README.md`. |
| `scripts/migrate.sh` (REMOVED 2026-07-13) | Deleted with migrations/, alembic, and the Postgres deps in the final cloud-lineage sweep. | — |
| `scripts/start-api.sh` (REMOVED 2026-07-13) | Deleted with the Gen-0 PostgreSQL lineage (`src/api`, `src/graph`, tenant CLI commands `index/stats/ego/impact/search`). Any external caller of those entry points was already dead or must migrate to the engine CLI (`query`/`graph` commands). | — |

### Retired Python surfaces (kept as removal records)

| Source | Invokes / assumes | Failure mode |
|---|---|---|
| ~~`tests/` (pytest), `pyproject.toml`, the `ruff` and `pytest-local` gates~~ (REMOVED 2026-08-06, retirement Phase 7) | **The dev test stack is deleted; `packages/aethyme` is 100% Rust.** All 145 pytest cases were accounted for before deletion: 41 MOVED with the eval scripts to `packages/aethyme-eval/tests/test_codex_eval_runner_contract.py`; 102 PORTED to `rust/crates/aethyme-cli/tests/` (`enhance_cli.rs` 15, `autofix_cli.rs` 15, `skill_templates.rs` 13, `explore_summary_cli.rs` 11, `ai_ready_cli.rs` 9, `skills_cli.rs` 6, `playground_hygiene.rs` 5, `commit_hygiene_cli.rs` 5, `local_workflow.rs` 4, `intents_cli.rs` 1) and `rust/crates/aethyme-testkit/tests/` (`docs_hygiene.rs` 13, `pr_template.rs` 5); 2 NOT ported — `test_require_local_engine_or_skip_{enforces_when_requested,skips_by_default}` tested the pytest harness's own skip helper, and the Rust harness has no skip path (a failed binary build asserts), so strict mode is unconditional and there is no helper left to test. One assertion changed rather than moved: `test_python_examples_syntax` compiled `python` fences with CPython, which no interpreter remains to do; it became "a Python-free package must carry no Python fences" (`docs_hygiene.rs::no_unverifiable_python_examples`). `tests/support/` is superseded by the `aethyme-testkit` crate (`bins`/`invoke`/`repos`/`paths`), which is `publish = false` and dev-dependency-only, so it can never enter `cargo install`. `pyproject.toml` held only pytest+ruff config and nothing in the product path read it. | `pytest packages/aethyme` and `ruff check packages/aethyme` now find nothing to do — replacement is `cargo test --workspace --manifest-path packages/aethyme/rust/Cargo.toml`. `packages/aethyme/.venv` is no longer created by CI or assumed by any gate; delete any local copy. The `pytest-local` gate's trigger set did not vanish with it — `cargo-test` gained `packages/aethyme/{skills,docs,rust/grammars}/**`, `scripts/eval/*.sh` and `.github/pull_request_template.md` so a markdown-only entry still runs the suites that assert over those files (the same silent-narrowing failure the retired gate's own comment recorded). |
| ~~`scripts/migration/`~~ (REMOVED 2026-08-01, retirement Phase 6) | The migration parity harness: `golden-diff.sh` (frozen CLI stdout), `enhance-golden.sh` (deployed artifact bytes), `golden-commands.txt`, `normalize-output.py`, plus the `quality-probe` and `fix-probe` dev bins in the aethyme-quality crate that the Phase 4/5 harnesses drove. All of it was capture-before-flip / compare-after-flip tooling for a migration with two sides; there is no Python side left to compare against, every command in `golden-commands.txt` is native, and no golden corpus was ever committed (goldens were captured to scratch dirs per phase). `golden-diff.sh` also hard-required `$PACKAGE_ROOT/.venv/bin/python`, and `normalize-output.py` was one of the last Python files in `packages/aethyme`. | Nothing regresses unguarded. CLI stdout shapes are held by Rust unit tests per command module plus the implementation-blind subprocess suites in `rust/crates/aethyme-cli/tests` (`ai_ready_cli.rs`, `autofix_cli.rs`, `explore_summary_cli.rs`, `local_workflow.rs`). Deployed artifact bytes — what `enhance-golden.sh` guarded — are held by `aethyme enhance verify` canonical-match checks, `aethyme-cli/tests/enhance_cli.rs`, and `scripts/eval/verify-playground.sh`. The probe bins had no consumer outside the deleted harness; removing them also shrinks what `cargo install` puts on PATH. |
| ~~`src/`~~ (REMOVED 2026-08-01, retirement Phase 6) | **The entire Python package is deleted.** Audited module by module before removal: `src/models/` — zero consumers anywhere, and its `__init__.py` already imported a `.graph` module that did not exist (a husk that would have failed on first import). `src/cli.py` — a Click group carrying zero commands since the Phase 5 flip; its only consumers were the retired migration/doc tooling and one in-process test of its own explore tombstone. `src/contracts/` (`versions.py`, `graph_export.py`, `run_metadata.py`, `eval_artifacts.py`) and `src/indexing/repository_snapshot.py` — a closed loop: consumed only by `tests/contracts/`, which existed only to test them. Nothing wrote the version stamps they defined (the engine stamps its own `aethyme-explore-v1` / `aethyme-verify-targets-v1`), `eval_artifacts.py` had zero references of any kind, and **`packages/aethyme-eval` imports nothing from `src/`** — so the cross-package dependency the plan anticipated does not exist and nothing needed moving into aethyme-eval (operator decision 6, 2026-08-01: aethyme-eval stays Python and owns its own inputs). The plan's Improvement #2 duplication is resolved by subtraction: with the Python definitions gone the Rust side is the only definition, and the sync test dies with the duplication. `src/indexing/engine.py` — a 72-line build-if-stale helper used only by `tests/local/test_local_workflow.py`; moved verbatim to `tests/support/engine_binary.py`, where it is labelled as dying with the tests/local Rust port rather than left behind to keep the `src.` import namespace alive. Also swept in the same commit: `requirements-dev.txt`, and the untracked `src/scorecard/`, `src/rendering/`, `tests/scorecard/` `__pycache__` husks that made deleted modules superficially importable (the plan's long-tail-artifact risk, seen live in the 2026-07-17 audit). | `python -m src.cli <anything>` now fails with `No module named src` — a **hard break**, announced in README and AGENTS.md rather than shimmed (plan risk item: announce, don't assume). Every command has a native equivalent: `aethyme <command>`. Anything importing `src.*` fails at import; the 2026-08-01 audit found zero importers outside this package's own tests. The output cache under `AETHYME_CACHE_DIR` / `/tmp/aethyme-cache` that `engine.py:CACHE_ROOT` documented has no writer any more; `aethyme repo clear-cache` is native and clears the engine's own caches. |
| ~~`python -m src.cli ai-ready`, `src/scorecard/*`~~ (REMOVED 2026-07-30, retirement Phase 4) | The Python `ai-ready` command, the scorecard package (models/engine/formatters/detectors), and `tests/scorecard/` were hard-deleted after the router flipped `ai-ready` to the native `aethyme-quality` crate (byte-parity verified on the Phase 4 corpus). Entry points at deletion: the router's Python delegation (now dispatches natively), `tests/scorecard/` (replaced by Rust unit tests + `aethyme-cli/tests/ai_ready_cli.rs` implementation-blind subprocess tests), and `scripts/docs/generate-docs.sh`'s generic Click-tree iteration (unaffected — it lists whatever commands remain). `record_scan_metrics` (`src/scorecard/metrics.py`) was an in-process Prometheus emitter that has been a silent no-op since the Phase 0 `prometheus-client` dependency purge; it wrote nothing on disk and had no reader, so it retires with no native replacement. `pydantic` left `pyproject.toml` in the same commit (scorecard was its last importer). | Out-of-repo scripts invoking `python -m src.cli ai-ready` now get Click's unknown-command error; migrate to `aethyme ai-ready` (identical flags/output). Anything importing `src.scorecard` fails at import; the 2026-07-30 audit found zero such importers outside the deleted command (autofix never imported it). |
| ~~`python -m src.cli autofix`, `src/autofixers/*`~~ (REMOVED 2026-08-01, retirement Phase 5) | The Python `autofix` command, the autofixers package (safety, patch, github, `_log`, and the 5 fixers), and `tests/autofixers/` were hard-deleted after the router flipped `autofix` to the native `aethyme-quality` crate (fix side; stdout, exit codes, produced unified diffs, and post-apply trees byte-parity verified on the Phase 5 corpus). Entry points at deletion: the router's Python delegation (now dispatches natively); `tests/autofixers/` (replaced by Rust unit tests plus `aethyme-cli/tests/autofix_cli.rs` implementation-blind subprocess tests); `scripts/migration/dump-cli-surface.py`'s Click-tree iteration (unaffected — the Commands table is simply empty now); and `scripts/docs/generate-docs.sh`'s generic Click-tree iteration (same). `src/cli.py:normalize_fixes`/`FixRecord` were autofix-only glue and went with it. **This was the last Python command: the Click tree now carries zero commands and the router delegates nothing.** `src/cli.py`, `src/contracts/`, `src/indexing/` (a 72-line engine build bootstrap for the dev test harness), and `src/models/` are all that remain of `src/`; they retire in Phase 6. | Out-of-repo scripts invoking `python -m src.cli autofix` now get Click's unknown-command error; migrate to `aethyme autofix` (identical argument, flags, stdout, exit codes and on-disk effects — see the Phase 5 entry in `cli-surface-v1.md` for the three recorded divergences). Anything importing `src.autofixers` fails at import; the 2026-08-01 audit found zero such importers outside the deleted command. The broker's lockfile ignore list (`aethyme-broker/src/leases.rs`) mirrored `safety.py:LOCK_FILES` by comment only and now points at `aethyme_quality::fix::safety::LOCK_FILES`. |
| ~~`evals/tools/*.toml`, `src/eval/orchestrator.py`, `tests/local/test_eval_warm_phase.py`~~ (REMOVED 2026-07-13) | The eval harness, its tool manifests, and the eval warm-phase contract were removed with the evaluation stack (knowledge preserved in `docs/architecture/eval-mining-notes.md`; sources at git ref `16cfa5e`). The **engine daemon commands themselves** (`aethyme-engine-cli daemon status/start`, `engine-daemon.log`, the `listening on` marker) remain live engine surface — they just no longer have an in-repo eval consumer. | Out-of-repo eval scripts that invoked `aethyme eval ...` or `aethyme methodology ...` now fail with unknown command; rebuild from the mining notes if needed. |

### Single `aethyme` entrypoint (#31, 2026-07-14)

| Source | Invokes / assumes | Failure mode |
|---|---|---|
| `pyproject.toml [project.scripts]` (REMOVED 2026-07-14) | The pip console script `aethyme = "src.cli:main"` was removed — `pip install -e .` no longer installs an `aethyme` executable that shadows the Rust router. `python -m src.cli <cmd>` is the only Python-side invocation. | Out-of-repo scripts calling the *pip* `aethyme` from inside an activated venv now resolve to the Rust router (PATH) or nothing; migrate them to `python -m src.cli`. In-repo audit 2026-07-14 found zero such callers. |
| Rust router `aethyme` (installed with the paired release archive or Homebrew; Cargo remains a contributor fallback) | **Every command is native; there is no delegation path.** Repository deployment and versioned `upgrade plan/apply` migrations use templates and migration logic embedded in the installed router, so they never resolve an Aethyme source root. Canonical deployment records a tracked repository schema; local-only deployment records an ignored schema. `root show/set` remains a legacy developer-compatibility surface, not an installation prerequisite. Explore auto-start spawns the sibling `aethyme-engine-cli` from the same install directory. | If the paired engine binary is missing or version-skewed, explore auto-start fails its sibling check. Broker use fails closed when an enrolled repository schema is missing, incomplete, or newer than the router. If the installed router is absent from PATH, deployed repository wrappers and hooks cannot invoke the product; repository policy itself remains clone-portable. |
| Router reader commands `aethyme explore-summary --from <file>` and `aethyme verify-targets --from <file>` (native router surface; `explore-summary` added in python-retirement Phase 5.5, 2026-08-01, `aethyme_enhance::explore_summary_cli`) | Both read the SAME saved `explore --format answer-json` temp file, so the deployed quick start makes one explore call and feeds two readers. `explore-summary` prints the compact decision surface (`safe_to_use_as_answer`, `trust_policy`, `subsystems`, `top_verification_targets`, `verification_steps`, `observability.readiness`) byte-identically to the retired `.venv/bin/python` heredoc — no `schema_version` key by contract decision. Usage errors exit 2, failures exit 1; `--from -` reads stdin. Consumed by `skills/aethyme/SKILL.md`, `skills/aethyme/AGENTS.md`, and `skills/aethyme/references/explore.md`. No frozen-surface doc covers it: `cli-surface-v1.md` froze the *delegated Click* surface and retired with it in Phase 6, so this row plus the tests below are the whole contract. | Renaming/removing either command silently breaks the deployed quick start in every enhanced repo (Class-3: fails only when an agent runs it). Field/order changes in `explore-summary` break the skill's "inspect only these fields" contract. Covered by `aethyme-cli/tests/explore_summary_cli.rs` (implementation-blind, drives the built binary), `aethyme_enhance::explore_summary_cli` unit tests, and `verify-playground.sh` template greps. |
| Generated broker protocol (`aethyme-enhance` crate `agents.rs:render_broker_protocol`, deployed into repo-root `AGENTS.md`/`CLAUDE.md` of broker-configured repos; same convention hand-condensed in this repo's root `CLAUDE.md`/`AGENTS.md`) | Bare `aethyme` from PATH (`broker status/start/adopt/leases claim/leases release/exec/submit/close`) plus gate-runner worker vars (`AETHYME_GATE_WORKER_ID`, `AETHYME_TEST_DB_SUFFIX`). Since 2026-07-17 the repo-local `rust/target/release/aethyme` fallback path is no longer rendered. v0.2.0 publishes the router and `aethyme-engine-cli` sibling in one archive and installs both through the stable-channel `install.sh`; source builds still require both Cargo packages. | Agents in enhanced repos run broker commands that fail with "command not found" if the router binary is renamed or the paired-install story breaks. Agents and gate configs can also fall back to shared files/databases if the lease, guarded-exec, and worker-suffix guidance drifts. Re-run `enhance deploy` after protocol wording changes; text assertions live in `aethyme-cli/tests/enhance_cli.rs`. |
| README quickstart and repository deployment guide | Presents the operator flow as: install the binary pair once -> run `aethyme deploy` in each repository -> commit policy -> retain `aethyme deploy verify` in CI -> run `aethyme broker quick-test` -> start and submit broker sessions. | Renaming/removing `deploy`, `deploy verify`, `broker quick-test`, `broker start`, or `broker submit` breaks first-run enrollment. Omitting committed policy leaves agents without mandatory broker instructions even when the executable is installed. |
| README operator verification row and broker CLI help (2026-07-28) | Presents `aethyme broker verify-loop` as the first-class broker E2E command, with `aethyme broker e2e` as an alias. The command snapshots the local integration head, runs quick-test, doctor, and source tests when in the Aethyme checkout, then warns/fails if integration moved. Human output is the documented surface; `verify-loop --json` is best-effort unless promoted into `docs/json-contracts.md`. | Renaming/removing `broker verify-loop` or the `broker e2e` alias breaks operator runbook verification and makes moving-integration proof stale again. |
| Broker CLI help for integration stability checks (2026-07-28) | Presents `aethyme broker integration wait-stable --seconds <n>` as the concise operator command for proving the local integration branch stayed on the same tip during a sampled quiet window. Human output is documented; `wait-stable --json` is best-effort unless promoted into `docs/json-contracts.md`. | Renaming/removing `broker integration wait-stable` brings back manual old-tip/current-tip comparisons after long checks, especially when active submitters move integration during a test run. |
| Broker CLI help for local source repair (updated 2026-08-23) | Presents `aethyme broker doctor --fix-version` as the explicit source-checkout repair for stale installed binaries. The command installs and independently verifies both `aethyme` and `aethyme-engine-cli` from one temporary integration worktree. `doctor --json` retains the legacy top-level repair fields and adds ordered `commands` plus per-component `steps`; this JSON remains best-effort unless promoted into `docs/json-contracts.md`. | Removing or making this implicit brings back the stale-binary manual reinstall loop; changing the repair source can install local WIP, and reporting success before every install/verify step passes can leave a mixed product version. |
| Push wrappers, CI pollers, webhook workers, and future Chau7/MCP delivery adapters for production PR follow-up (2026-07-31) | `aethyme broker pr check [--target <branch>] [--pr <number>] [--agent <name>] [--dispatch] [--json]` reads PR state via `gh`, stores a deterministic activity fingerprint in `.aethyme/broker.db`, writes a bounded prompt under `.aethyme/run/pr-follow-up/`, and may reuse/spawn a `Push2prod` broker session when `--dispatch` is explicit. JSON is provisional until promoted into `docs/json-contracts.md`. | Renaming/removing `broker pr check`, changing marker strings (`thumbs_up`, `looking`, `none`), or changing decision/dispatch enum strings breaks automation that decides whether to send a prompt to an existing tab or start a new agent after a successful push. Git has no native post-push hook, so callers must run this from a wrapper/controller after push success rather than from `pre-push`. |
| Stable broker JSON command contracts (`docs/json-contracts.md`) | `aethyme broker status --json`, `aethyme broker integration status --json`, `aethyme broker events --json`, `aethyme broker metrics --json`, and `aethyme broker submit --json`. | Scripts and dashboards consuming broker state can break if these commands, field names, or enum strings are renamed/removed without a versioned contract change. |
| `.github/workflows/release.yml` (tag `v*` push) | Validates the tag against the workspace version; builds `aethyme` and `aethyme-engine-cli`; checks both embedded versions; packages and extracts the pair on each supported target; runs both `--version` commands plus `aethyme broker quick-test`; generates archive/installer checksums and a compatibility manifest from the exact source SHA; signs and verifies the manifest with the release-workflow OIDC identity; then publishes every asset with the checked-in upgrade guide. | Renaming either binary, changing the archive member order/names, drifting the manifest schema or compatibility constants, removing quick-test, changing the workflow identity, or letting `Cargo.lock` go stale breaks tag releases before publication. |
| Root `install.sh`, published as a versioned and `releases/latest` asset | Discovers stable v0.2+ releases through `release-manifest.json`; maps macOS/Linux host architecture to a supported target; downloads the exact versioned paired archive and checksum; validates checksum, archive members, and both binary versions; stages both executables under `~/.local/bin` by default. `--verify-signature` additionally verifies the Sigstore bundle and binds the reviewed installer file to its signed digest. | Renaming manifest fields/assets, either archive member, the GitHub workflow identity, or the stable channel breaks installation. The offline `release_installer` fixture covers successful update, bad archive checksum, matching signed installer, and tampered installer refusal. |

### Installed git hooks (`aethyme broker hooks install`, 2026-07-17)

| Source | Invokes | Failure mode |
|---|---|---|
| `<git-common-dir>/hooks/pre-commit` and `post-commit` marker blocks (`# >>> aethyme hooks >>>`), written by `aethyme broker hooks install` into **every repo where an operator opted in** — canonical template: `rust/crates/aethyme-broker/src/hooks.rs:hook_block` | `"<abs-path-to-aethyme-captured-at-install>" broker hooks pre-commit` (blocking, `\|\| exit $?`) and `... broker hooks post-commit` (`\|\| true`) | **Renaming/removing the `broker hooks pre-commit` subcommand blocks ALL commits** in every repo with hooks installed: the shim treats any non-zero exit — including "unknown subcommand" — as a failed gate. `post-commit` degrades silently. A moved/deleted *binary* is handled gracefully by the shims (warn-and-pass / silent skip). Escape hatches: `git commit --no-verify`, `aethyme broker hooks uninstall`, or deleting the marker block by hand. |
| Repository-owned pre-push hooks following `docs/guides/host-resource-coordination.md` | `aethyme broker gates pre-push "$@"` with Git's ref-update protocol preserved on stdin. Aethyme deliberately does not install this hook. | Renaming/removing `broker gates pre-push`, consuming stdin before delegation, or changing its clean-current-HEAD proof can either block opted-in pushes or validate a tree other than the outgoing tip. Repositories may provide an explicit isolated fallback when the binary is absent; they must not retry after Aethyme starts and partially executes. |

### Rust `aethyme` router ↔ Python daemon (REMOVED 2026-07-13, issue #29)

| Source | Invokes / assumes | Failure mode |
|---|---|---|
| ~~`rust/crates/aethyme-engine/src/bin/aethyme.rs` ↔ `src/daemon.py`~~ | Both sides removed in the same commit (the coordinated change this row demanded). The router's repo-daemon subcommand, Python-daemon socket fallback, and socket-path logic are gone. `src/daemon.py` and its `cli.py` registration are deleted. `aethyme explore` is a native-engine-owned surface. Non-explore commands no longer fall through to Python at all — the delegation path went with `src/` in Phase 6 (2026-08-01). Since 2026-07-14 (#31) explore runs **in-process** in the router via `aethyme_engine::explore_cli`, auto-starting the engine daemon (spawning the sibling `aethyme-engine-cli daemon serve`). | Any out-of-repo script calling `aethyme daemon start/stop/status` (repo-socket flavor, `aethyme.sock` / `aethyme-socket.path`) now falls through to the Python CLI, which reports an unknown command. The **engine** daemon (`aethyme-engine-cli daemon ...`, `engine-<hash>.sock`, eval warm phase) is a separate live contract and is unaffected — see the engine-daemon rows above. |

| Workflow | Calls | Failure mode |
|---|---|---|
| `.github/workflows/aethyme-local-tests.yml` | **DEV lane, no Python since python-retirement Phase 7 (2026-08-06).** Builds `aethyme`, `aethyme-engine-cli` and `aethyme-graph-index`, then runs `cargo test -p aethyme-cli -p aethyme-testkit` — the implementation-blind suites ported from the retired `tests/local` pytest tree. The pytest strict-engine lane is gone with it: `aethyme_testkit::bins` asserts on a failed build instead of skipping, so strict mode is unconditional and there is no second lane to run. | Suites can fail if a binary target is renamed (they resolve the three bins by name). Now carries a product-path signal it never had: this lane has no interpreter at all. |
| `.github/workflows/oss-ci.yml` | The `product-path-no-python` job installs the router, engine sibling, and graph indexer, then runs canonical `aethyme deploy` and read-only `deploy verify` behind a sandbox PATH with no interpreter. It rejects source-build and target-checkout paths in generated policy before exercising the remaining product surface. | Catches interpreter regressions, missing embedded deployment assets, non-portable policy, or renamed crate/bin/command surfaces. |
| `.github/workflows/cross-process-contract.yml` | Builds the router from the checkout (`cargo run … -p aethyme-cli --bin aethyme`) and runs `aethyme broker check-contract --base ... --pr-body ...`; the checker treats this document as the source of truth for protected entry-point strings. Native since python-retirement Phase 6 (2026-08-01). | PR contract check becomes stale if this registry misses a consumer or if the PR-template contract language changes without updating `aethyme-broker/src/contract_check.rs`. Breaks if the `broker check-contract` subcommand or the `aethyme` bin target is renamed. |
| ~~`scripts/check-cross-process-contract.py`~~ (REMOVED 2026-08-01, retirement Phase 6) | The Python contract checker was ported to `aethyme-broker/src/contract_check.rs` and is now reached as `aethyme broker check-contract`. Behaviour is unchanged and was verified byte-identical (stdout, stderr, exit codes) against the Python implementation over the clean / undeclared-removal / declared-decision cases before deletion. Entry points at deletion: `.github/workflows/cross-process-contract.yml` and the `cross-process-contract` gate in `.aethyme/gates.toml` (both updated in the same commit); `tests/local/test_cross_process_contract_check.py` (replaced by unit tests inside the Rust module, including the two live-registry assertions). The port had to precede the `src/` deletion — otherwise the migration's largest removal would have landed with its own guard switched off. | Out-of-repo automation invoking `python scripts/check-cross-process-contract.py` gets "No such file". Migrate to `aethyme broker check-contract` (same `--base` / `--pr-body` flags, same exit codes; `--consumers-doc` added to override the registry path). |
| `.github/workflows/aethyme-gates.yml` | Sets up an interpreter (for the `pytest-aethyme-eval` gate only — that gate provisions its own venv inside the tree under test since python-retirement Phase 7; it no longer creates `packages/aethyme/.venv`, which retired with the pytest stack), builds the router binary (`cargo build --release --manifest-path packages/aethyme/rust/Cargo.toml --bin aethyme`), then runs `aethyme broker gates run --all` from the repo root — the same `.aethyme/gates.toml` + runner the broker uses to verify submissions (single definition of "verified"). Emits no new event kinds (only the pre-existing `gate.*` events from the shared runner). | Breaks if the `broker gates run --all` subcommand, the `aethyme` bin target, or `.aethyme/gates.toml` is renamed/moved. No longer breaks on a venv layout: no gate borrows `$MAIN`'s interpreter any more. Convergence lane: the pre-existing test workflows above stay until this lane proves equivalent; deletion is a separate decision. |
| `packages/aethyme/.github/workflows/*` (REMOVED 2026-07-09) | The package-level workflow set (`ci.yml`, `evals.yml`, `performance.yml`, `cd.yml`, `aethyme-example.yml`) and the `aethyme-scorecard` action were deleted in the Phase 0 truth cleanup. GitHub never executes workflows outside the repo-root `.github/workflows/`, `evals.yml`/`ci.yml` referenced test paths (`tests/evals/`, `tests/integration/`) that no longer exist, and the scorecard action ran `pip install aethyme-cli` — a PyPI name this repo does not publish. | Any external repository referencing `uses: .../packages/aethyme/.github/actions/aethyme-scorecard@main` was already broken by the wrong pip package name; such callers must pin an old commit or migrate to invoking `aethyme ai-ready` directly. |

### Externally deployed runtime files (under `~/Downloads/Repositories/Playground/`)

These are files in *separate repos* (the playground clones), put there
by `deploy_skills` / `aethyme enhance deploy`. For benchmark clones, static skill
deployment remains the compatibility path; for normal repositories,
`aethyme deploy` is the primary path because it also configures and certifies
the broker contract.

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
| `explore` non-usage-boundary intents | Read-only | Native `task_localization_query`, `behavior_localization_query`, and auto-selected explore flows read graph/navigation data from redb and report redb store freshness plus `surface_flow_graph` coverage in observability. Coverage is separate from freshness: a fresh store can still report `source_present_not_indexed` or `partially_indexed` for missing Surface/Flow families such as edge/proxy ingress. Surface/Flow `indexed` means semantic node/edge evidence; path-only fragment evidence is reported separately as `path_indexed`. They do not build `RepositoryMap` in production. |
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
