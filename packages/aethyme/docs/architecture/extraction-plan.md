# Eval-stack extraction plan

Phased plan for extracting Aethyme's eval framework (`src/eval/`,
`src/eval/tools/`, `evals/tools/`, `aethyme-eval-ui/`, plus supporting
artifacts) into a standalone, potentially publishable package.

**Status (as of 2026-05-18):** sized but not committed. The session that
shipped the tool-adapter manifest system (`d27d931` → `c64f98b`) cut the
total extraction cost roughly in half compared to the initial audit at
session start. This plan captures what would actually be done if/when
the extraction happens, in three reversibility-graded phases.

## Architecture this plan assumes

The eval framework now consists of:

- `src/eval/` — orchestrator, prepare flows (bug_fix / explain_repo /
  navigation_ctf), prompts, scoring, schemas, runner, telemetry.
- `src/eval/tools/` — `ToolAdapter` Protocol, manifest loader, registry.
- `src/eval/tools/{base,manifest,registry}.py` — the load-bearing seam.
- `evals/tools/*.toml` — per-tool manifests with mandatory
  `[notes].condition_mapping` audit trail.
- `src/scorecard/` — independent AI-readiness scorecard product
  (extractable separately or as part of the bundle).
- `src/contracts/` — small shared types (`eval_artifacts.py`,
  `run_metadata.py`, `versions.py`) — ~200 LoC.
- `packages/aethyme-eval-ui/` — React + FastAPI local UI; reaches into
  `src/eval/` via `sys.path.insert` today.
- `docs/guides/eval-protocol.md` — the methodology contract.
- ~161 eval-related tests under `tests/local/test_eval_*` and
  `tests/scorecard/`.

The phased plan below addresses each of these surfaces.

## Phasing principle

Each phase is reversible until the last. Each phase produces a
checkpoint where the framework still works end-to-end. Stopping after
Phase 1 or Phase 2 leaves a meaningfully-improved in-tree framework;
only Phase 3 is the irreversible commit.

---

## Phase 1 — Mechanical cleanup

**Goal:** Lower the entropy of the existing in-tree framework. Every
change here is reversible, low-risk, and improves clarity whether or
not Phase 3 ever happens. This phase can be done casually between
other work.

**Validation gate:** all eval-related tests pass (~161 today); one live
`aethyme eval run` smoke still produces correct results.

| # | Item | Rationale | Estimate |
|---|---|---|---:|
| 1.1 | Rename `AETHYME_CONDITIONS` → `TOOL_USING_CONDITIONS` in `src/eval/repos.py` (+ all references) | "AETHYME_*" naming is historical, not semantic — these are "conditions that use a tool" | 30 min |
| 1.2 | Rename `AETHYME_PACKAGE_ROOT` constant or wrap in a `host_package_root()` accessor | What it really means is "the host package the eval lives in" | 30 min |
| 1.3 | Consolidate the three identical "inline-warm" blocks (`bug_fix.py`, `explain_repo.py`, `navigation_ctf.py`) into a shared `_inline_warm_if_needed(adapter, repo)` helper | Today the same ~15-line block is copy-pasted three times | 45 min |
| 1.4 | Pin `evals/tools/graphify.toml [source].ref` to a specific commit SHA (today `main`) | Reproducibility requires pinning before any published comparison | 15 min |
| 1.5 | Pin `evals/tools/aethyme.toml [version].command` to also output the git SHA explicitly | Currently relies on `git rev-parse HEAD`; this should be standardized | 15 min |
| 1.6 | Audit `_LEVERAGE_MINIMAL_POINTER` and similar Aethyme-text hardcoded constants in `bug_fix.py` | Either move into Aethyme's manifest as `[conditions.leverage].prompt_addendum` or accept as historical Aethyme-config | 30 min |
| 1.7 | Investigate the `cross-process-consumers.md` mystery (session 2026-05-18) | The initial gitStatus showed it as modified but it's now clean with no audit trail — needs a real explanation | 30 min |
| 1.8 | Clean up `negative-context` for non-Aethyme tools (currently auto-skipped as leverage-replay) | The "fall back to leverage replay" semantic is methodologically muddy; either the condition is genuinely skipped (cleanest) or gets a different test | 1 hour |
| 1.9 | Add a `make audit-aethyme-references` target that greps for hardcoded "aethyme" strings | Pre-requisite for tracking decoupling progress in Phase 2 | 30 min |
| 1.10 | Document why the parity tests' contract is "byte-identical, not just functional" | This methodological choice is load-bearing for any future extraction; today it's only in commit messages | 30 min |

**Phase 1 total: ~5–6 hours of focused work.**

---

## Phase 2 — Decoupling prep

**Goal:** Make Phase 3 mechanical rather than architectural. Each item
either removes a specific obstacle to extraction or surfaces a
methodological decision that needs to be made *before* the move, not
during.

**Validation gate:** the framework can run with `tool=aethyme` via
subprocess-only (no direct `build_task_pack` calls from
`prepare_bug_fix_benchmark` et al.) AND produce byte-identical results
to the legacy path. This is the proof that "Aethyme as just-another-
tool" works.

| # | Item | Rationale | Estimate |
|---|---|---|---:|
| 2.1 | **Reshape `prompts.py` to template the leverage hint per tool** | The diagnostic-eval prompts (`dead-code`, `bug-fix-1`, etc.) currently hardcode "Use Aethyme tools to navigate the repository graph." Needs `tool_name` parameter → "Use {tool_name} tools..." | 2 hours |
| 2.2 | **Convert Aethyme's manifest from `in_tree=true` to a regular git-cloned manifest** | The methodological "Aethyme is just another tool" milestone. The manifest clones the Aethyme repo and invokes its CLI via subprocess. Today's `in_tree=true` is a privilege that should not survive extraction. | 4 hours |
| 2.3 | **Decide the legacy-direct-Python-path question** and document it | Two options: (a) preserve byte-identical-historical-runs forever (keep the `tool=None` direct-Python fallback) or (b) full extraction cleanness (remove the path; historical comparisons become approximate). Stake out a position. | 2 hours |
| 2.4 | **Build `eval_config.py` (or similar) as single source of truth for host-package settings** | Currently `AETHYME_PACKAGE_ROOT`, the engine binary path, etc. are scattered. One config file → easier to swap when extraction happens. | 2 hours |
| 2.5 | **Implement the warm-cost measurement** | The Graphify-vs-Aethyme cost comparison silently hides Graphify's warm spend. Capturing warm-step cost separately (or counting it into total) closes the honest measurement gap. Strong methodology piece for extraction. | 4 hours |
| 2.6 | **Test the Aethyme manifest's `aethyme install` self-installer** (the equivalent of Graphify's `claude install`) | Aethyme needs its own per-clone register subcommand. Once present, the manifest can do `cd {{TARGET_REPO}} && aethyme install` symmetric with Graphify. | 2 hours |
| 2.7 | **Convert `aethyme-eval-ui` server's `sys.path.insert` to a proper import** | Currently the FastAPI server reaches into `packages/aethyme/src/eval/`. Convert to importing via pip-install path so it works in both in-tree and extracted mode. | 4 hours |
| 2.8 | **Make `repos.py:create_condition_repos` source-of-clones tool-aware** | Currently `source = target.aethyme_path` is wired in the orchestrator's prepare cli_cmd. For `tool=non-aethyme`, source should probably be `target.control_path` (tool gets installed via [register]). | 2 hours |
| 2.9 | **Move `src/contracts/` to a vendor-able location** | The ~200 LoC of contracts is needed by both eval and the (potentially extracted) framework. Either vendor or convert to a shared dep. | 1 hour |
| 2.10 | **Write the canonical methodology doc** (`docs/architecture/methodology.md`) | The condition matrix, mandatory `[notes].condition_mapping`, parity-test discipline, warm-cost surfacing, 4-section diagnostic report — these are the headline if you ever extract publicly. Pre-write the doc; extraction becomes "move package + reference this doc." | 4 hours |
| 2.11 | **Build a comprehensive cross-tool regression test** | One run that exercises bug-fix + explain-repo + navigation-ctf + dead-code (via prompts_writer with tool support post-2.1) with both `aethyme` and `graphify`. Locks the framework's correctness across both tools before extraction. | 1 day (+ ~$30 in eval spend) |

**Phase 2 total: ~3-4 days of focused work + one large regression run.**

**Critical-path items inside Phase 2:**

- **2.3 (legacy-path decision) gates 2.2 (Aethyme manifest reshape).** Decide first.
- **2.1 (prompts.py templating) gates 2.11 (comprehensive regression run).** Diagnostic evals can't run with non-aethyme tools until prompts.py is reshaped.
- **2.7 (UI import strategy) can be done in parallel with everything else.**

---

## Phase 3 — Decoupling

**Goal:** Move the framework into its own repository / package. Aethyme
depends on the new package the same way any external tool does (pip
dep + manifest).

**Validation gate:** Aethyme's CI still runs all its evals via the (now
external) framework, with results byte-identical to pre-extraction
baseline runs.

| # | Item | Rationale | Estimate |
|---|---|---|---:|
| 3.1 | Create the new repo with chosen structure + name | Irreversible once published; choose carefully | 1 hour |
| 3.2 | Use `git filter-repo` (or similar) to extract the relevant subdirs *with history* | Preserves blame / commit context; cleaner for any open-source publication | 2-4 hours |
| 3.3 | Rewrite imports throughout (`src.eval` → `<new_pkg>.*`, etc.) | Mechanical sed pass + spot-check | 2 hours |
| 3.4 | Move all eval tests; verify they pass against the new package | The parity tests are the load-bearing assertion that nothing changed semantically | 2 hours |
| 3.5 | Set up CI on the new repo | Probably mirror Aethyme's existing CI structure | 2 hours |
| 3.6 | Update the UI's pip-install path | Already prepped in 2.7; this is just flipping the switch | 1 hour |
| 3.7 | Publish to PyPI / GitHub | Pin Aethyme's `pyproject.toml` to a specific version | 1 hour |
| 3.8 | Update Aethyme's `pyproject.toml` and `tests/` to depend on the new package | Aethyme's CI now installs the framework, doesn't ship it in-tree | 2 hours |
| 3.9 | Move `docs/guides/eval-protocol.md` and `docs/architecture/methodology.md` to the new repo | These are the user-facing documents | 1 hour |
| 3.10 | Write `docs/migration.md` in the new repo for adopters | "How to add your tool to this eval framework" — the manifest's `[notes]` field becomes the centerpiece | 2 hours |
| 3.11 | Smoke-test against the playground from the new repo | One full eval run via the now-external framework | 2 hours + ~$5 |
| 3.12 | Bidirectional verification: Aethyme's historical eval results don't change | Run a known-good historical eval through the extracted framework; assert byte-equivalence | 2 hours + ~$5 |
| 3.13 | Delete the now-redundant `packages/aethyme-eval-ui/` from Aethyme (lives in the new repo) | Cleanup of the carved-out surface | 1 hour |

**Phase 3 total: ~3 days of focused work + ~$10 in verification spend.**

---

## Summary

| Phase | Effort | Reversibility | Value if you stop here |
|---|---|---|---|
| Phase 1 | ~5-6 hours | Fully reversible | Cleaner in-tree framework; no Aethyme-specific naming creep; better debuggability |
| Phase 2 | ~3-4 days + $30 | Mostly reversible | Framework is multi-tool with no Aethyme privilege; methodology is documented; warm-cost is measured; comprehensive regression test exists |
| Phase 3 | ~3 days + $10 | Irreversible (or expensive) | The eval framework is a separate, publishable, externally-extensible package |

**Cumulative cost:** ~7-8 days focused work + ~$40 eval spend.

## Three scenarios for how this might unfold

1. **Stop after Phase 1.** The cleanup is valuable on its own. The
   framework is more grep-able and less Aethyme-name-creep-laden.

2. **Stop after Phase 2.** The *sweet spot* if you don't want to publish
   externally. Aethyme has a fully tool-neutral framework in-tree, the
   methodology is documented, you can add competitor tools by dropping
   manifests, and the cross-tool regression test proves it works.

3. **Do Phase 3.** You have an open-source eval harness that other
   tools can adopt. The methodology doc from 2.10 is the value prop;
   the runner code is the carrier.

## Cross-references

- Initial extractability audit: chat history of session 2026-05-15 (3
  flavors A/B/C; the manifest system absorbed most of Flavor A's
  initial complexity)
- Tool-adapter manifest system architecture: commits `d27d931`,
  `0025b12`, `06edac0`, `d47a137`, `c9f773b`, `6997b55`, `c9e2dec`,
  `c64f98b` (2026-05-15 through 2026-05-18)
- Parity-test discipline: `tests/local/test_eval_navigation_context_adapter_parity.py`,
  `tests/local/test_eval_explain_repo_adapter_parity.py`,
  `tests/local/test_eval_navigation_ctf_adapter_parity.py`
- Manifest validation contract: `src/eval/tools/manifest.py`
- The `[notes].condition_mapping` mandatory-field rule: same file
