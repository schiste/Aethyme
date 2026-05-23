# AethymeBench extraction plan

Phased plan for extracting Aethyme's eval framework (`src/eval/`,
`src/eval/tools/`, `evals/tools/`, `aethyme-eval-ui/`, plus supporting
artifacts) into a standalone, publishable package: **AethymeBench**.

**Status (as of 2026-05-19):** sized, scoped, and architecturally
locked. The session that shipped the tool-adapter manifest system
(`d27d931` → `c64f98b`) cut the total extraction cost roughly in half
compared to the initial audit. The 2026-05-18/19 planning round then
locked the remaining strategic and methodological decisions. This plan
captures what will be done, in four reversibility-graded stages.

## Locked decisions (planning round 2026-05-18/19)

| Topic | Decision |
|---|---|
| **Package name** | **AethymeBench** (pip: `aethyme-bench`, import: `aethyme_bench`) |
| **License** | **MIT** |
| **Distribution** | **GitHub-only at first.** PyPI deferred until usage demands it. |
| **Contributions** | Open from day one. Solo maintainer + agents. |
| **Versioning** | **CalVer** (`YYYY.0M.0D`, e.g. `2026.05.19`). |
| **Methodology drift** | 3-tier policy mapped onto CalVer segments (see below). |
| **Run-output anchor** | Every run output stamps a `methodology_hash` (see below). |
| **Plugin axes** | **Five:** tools, eval types, targets, backends, scorers. |
| **In-tree privilege** | Generalized into a `tool` variable any tool developer can use. |
| **Extraction path** | **Four stages**, not three. Stage 2.5 = monorepo subdir soak. |
| **Pre-split history** | Preserved in `HISTORY.md` in the extracted repo. |
| **Chau7 dependency** | Documented as a first-class dependency in adopter docs. |
| **eval-runs/** | gitignored in the public repo. |
| **Scorecard** | Ships with the framework; web-app is its primary home. |
| **Upstream fixes** | If a fix is found during decoupling, upstream it to Aethyme. |

## Architecture this plan assumes

The eval framework now consists of:

- `src/eval/` — orchestrator, prepare flows (bug_fix / explain_repo /
  navigation_ctf), prompts, scoring, schemas, runner, telemetry.
- `src/eval/tools/` — `ToolAdapter` Protocol, manifest loader, registry.
- `src/eval/tools/{base,manifest,registry}.py` — the load-bearing seam.
- `evals/tools/*.toml` — per-tool manifests with mandatory
  `[notes].condition_mapping` audit trail.
- `src/scorecard/` — independent AI-readiness scorecard product
  (extracted as part of the bundle; web-app remains its primary home).
- `src/contracts/` — small shared types (`eval_artifacts.py`,
  `run_metadata.py`, `versions.py`) — ~200 LoC.
- `packages/aethyme-eval-ui/` — React + FastAPI local UI; reaches into
  `src/eval/` via `sys.path.insert` today.
- `docs/guides/eval-protocol.md` — the methodology contract.
- ~161 eval-related tests under `tests/local/test_eval_*` and
  `tests/scorecard/`.

The phased plan below addresses each of these surfaces and adds the
five plugin axes the planning round identified as required for genuine
extraction.

## The five plugin axes

A standalone eval framework needs more than "drop in another tool";
each of these is a point where Aethyme-specifics currently leak into
the framework:

1. **Tools** — already pluggable via `evals/tools/*.toml`. ✓
2. **Eval types** — currently `bug_fix`, `explain_repo`,
   `navigation_ctf`, `dead_code` are hardcoded as separate prepare
   modules. Needs a `EvalTypeAdapter` Protocol so adopters can register
   new eval types without forking the framework.
3. **Targets** — currently `src/eval/targets.py` is a fixed dict keyed
   on slugs ("grc", "mediawiki"). Needs to be loadable from a config
   file the adopter ships, so AethymeBench's framework code is
   target-agnostic.
4. **Backends** — currently `claude` and `codex` are wired into the
   orchestrator with branching. Needs a `BackendAdapter` Protocol
   covering tab orchestration, prompt submission, output collection,
   and pricing tables.
5. **Scorers** — currently each eval type has its own scoring function
   inside its prepare module. Needs a `ScorerAdapter` Protocol so the
   same eval type can be scored multiple ways and so adopters can ship
   custom scorers for their domain.

Stage B's plan items map onto these five axes explicitly (see 2.12–2.16).

## Methodology drift policy (3-tier, mapped onto CalVer)

The hard constraint from the planning round (Q23): **eval scenarios
are units across versions.** A `bug-fix-1` run from 2026-03 must
remain comparable to a `bug-fix-1` run from 2026-08 — otherwise the
framework has no longitudinal value.

CalVer segments carry semantic meaning:

| Segment | Change class | Cross-version comparability |
|---|---|---|
| **Year** (`2027.*.*`) | Methodology break — condition matrix changes, scoring overhaul, schema redesign. | Not comparable. New baseline year. |
| **Month** (`2026.06.*`) | Scorer adjustments, prompt rewording, weights tweaks, methodology snapshot updates. | Same scenario can still be compared with a methodology-hash caveat. |
| **Day/patch** (`2026.05.20`) | Bugfixes, infra, tool-adapter additions, performance work, doc updates. | Fully comparable. |

This makes the version string itself the first line of "is this run
comparable to that run?" The `methodology_hash` (next section) is the
authoritative second line.

## methodology_hash — the run-output anchor

Every AethymeBench run stamps a `methodology_hash` into its output
JSON. The hash is computed at run-start by hashing the concatenation of:

- The contents of the **prompts** registry (all prompt templates).
- The contents of the **scorers** module (scoring logic + weights).
- The active **condition matrix** (which conditions are enabled for
  this eval type).
- The active **manifest digest** for each tool in the run (sha256 of
  the `.toml` file).
- The CalVer version of AethymeBench itself.

The hash is a short SHA256 prefix (12 hex chars). Two runs with
identical `methodology_hash` are guaranteed comparable. Two runs with
different hashes are comparable only with a documented diff — the
framework ships a `aethyme-bench methodology diff <hash_a> <hash_b>`
helper.

Paired with a **golden-file methodology snapshot** under
`docs/methodology/snapshots/<methodology_hash>.json` that fully captures
prompts/scorers/condition-matrix at that hash. Re-running an old
methodology becomes a deterministic operation, not a forensic one.

## The `tool` variable — Q16 generalization

The existing plan had Phase 2.2 as "convert Aethyme's manifest from
`in_tree=true` to a regular git-cloned manifest" — i.e. *remove* the
privilege as a methodological cleanup.

The planning round refined this: **`in_tree=true` is a useful pattern
for any tool developer working on their own tool**, not just Aethyme.
The Aethyme privilege becomes a generic `tool` variable that any
contributor can set in their own fork:

```toml
# evals/tools/aethyme.toml
[source]
in_tree = true
tool = "aethyme"  # the framework recognises this as the "self" tool

# evals/tools/my-new-tool.toml (some other developer's fork)
[source]
in_tree = true
tool = "my-new-tool"  # they're developing their tool against AethymeBench
```

Effect: the framework's published behavior is "if your `[source].tool`
matches an environment variable `AETHYMEBENCH_SELF_TOOL=<name>`, the
manifest is read in-tree from `$PWD` instead of cloned." Aethyme sets
`AETHYMEBENCH_SELF_TOOL=aethyme` in its own CI. Adopters set it to
their own tool name if they want the same iteration speed.

This turns what was a privilege into a generic affordance.

## Phasing principle

Each stage is reversible until the last. Each stage produces a
checkpoint where the framework still works end-to-end. Stopping after
any stage leaves a meaningfully-improved system; only Stage C is the
irreversible publication step.

---

## Stage A (Phase 1) — Mechanical cleanup

**Goal:** Lower the entropy of the existing in-tree framework. Every
change here is reversible, low-risk, and improves clarity whether or
not later stages happen. Can be done casually between other work.

**Validation gate:** all eval-related tests pass (~161 today); one live
`aethyme eval run` smoke still produces correct results.

| # | Item | Rationale | Estimate |
|---|---|---|---:|
| 1.1 | Rename `AETHYME_CONDITIONS` → `TOOL_USING_CONDITIONS` in `src/eval/repos.py` (+ all references) | "AETHYME_*" naming is historical, not semantic — these are "conditions that use a tool" | 30 min |
| 1.2 | Rename `AETHYME_PACKAGE_ROOT` constant or wrap in a `host_package_root()` accessor | What it really means is "the host package the eval lives in" | 30 min |
| 1.3 | Consolidate the three identical "inline-warm" blocks (`bug_fix.py`, `explain_repo.py`, `navigation_ctf.py`) into a shared `_inline_warm_if_needed(adapter, repo)` helper | Today the same ~15-line block is copy-pasted three times | 45 min |
| 1.4 | Pin `evals/tools/graphify.toml [source].ref` to a specific commit SHA (today `main`) | Reproducibility requires pinning before any published comparison | 15 min |
| 1.5 | Pin `evals/tools/aethyme.toml [version].command` to also output the git SHA explicitly | Currently relies on `git rev-parse HEAD`; this should be standardized | 15 min |
| 1.6 | ~~Audit `_LEVERAGE_MINIMAL_POINTER` and similar Aethyme-text hardcoded constants in `bug_fix.py`~~ **DEFERRED → 2.1.** Structural plumbing already exists: `bug_fix.py:584-601` branches on `tool_name != "aethyme"` and emits a generic pointer. The Aethyme-specific *text* at `bug_fix.py:546-552` (`_LEVERAGE_MINIMAL_POINTER`) and the leverage-hint string in `prompts.py` are what still need to become tool-provided; that work is exactly what 2.1 (prompt templating per tool) does. Doing it twice — once in Stage A as a one-off rewrite, again in Stage B as the Protocol-driven version — would be wasted effort. | deferred → 2.1 |
| 1.7 | Investigate the `cross-process-consumers.md` mystery (session 2026-05-18) | The initial gitStatus showed it as modified but it's now clean with no audit trail — needs a real explanation | 30 min |
| 1.8 | ~~Clean up `negative-context` for non-Aethyme tools (currently auto-skipped as leverage-replay)~~ **DEFERRED → 2.17.** Two coupled problems: (a) the preamble at `bug_fix.py:602-606` hardcodes `"Use Aethyme tools to navigate the repository graph."` and `_NEGATIVE_NAV_CONTEXT_PATH`, and (b) the leverage-replay fallback semantic is methodologically muddy regardless of tool. Both need a single Protocol-aware fix, not a Stage A bandaid that would be undone by 2.12-2.15. Captured as new Stage B item 2.17. | deferred → 2.17 |
| 1.9 | Add a `make audit-aethyme-references` target that greps for hardcoded "aethyme" strings | Pre-requisite for tracking decoupling progress in Stage B | 30 min |
| 1.10 | Document why the parity tests' contract is "byte-identical, not just functional" | This methodological choice is load-bearing for any future extraction; today it's only in commit messages | 30 min |
| 1.11 | Scope `graphify extract` to source-only (exclude `.git/`, `.codex/`, `node_modules/`) — carryover from task #24 | Avoid graph-extracting Aethyme's own data when graphify runs against an Aethyme-installed repo | 30 min |

**Stage A total: ~5–6 hours of focused work.**

---

## Stage B (Phase 2) — Decoupling prep

**Goal:** Make Stage 2.5 mechanical rather than architectural. Each
item either removes a specific obstacle to extraction or surfaces a
methodological decision that needs to be made *before* the move, not
during.

**Validation gate:** the framework can run with `tool=aethyme` via
subprocess-only (no direct `build_task_pack` calls from
`prepare_bug_fix_benchmark` et al.) AND produce byte-identical results
to the legacy path. This is the proof that "Aethyme as just-another-
tool" works.

### Original 2.1–2.11 (existing items)

| # | Item | Rationale | Estimate |
|---|---|---|---:|
| 2.1 | **Reshape `prompts.py` to template the leverage hint per tool** | The diagnostic-eval prompts (`dead-code`, `bug-fix-1`, etc.) currently hardcode "Use Aethyme tools to navigate the repository graph." Needs `tool_name` parameter → "Use {tool_name} tools..." | 2 hours |
| 2.2 | **Generalize Aethyme's `in_tree=true` privilege into a `tool` variable** (revised per Q16) | The pattern stays; what changes is who can use it. Any developer can set `[source].in_tree = true` and `[source].tool = "<their-tool>"` and the framework respects it when `AETHYMEBENCH_SELF_TOOL` matches. Aethyme keeps its iteration speed; adopters get the same affordance. | 4 hours |
| 2.3 | **Remove the legacy direct-Python path** (decision: 2026-05-19, Option A — full extraction cleanness). After 2.16 lands and golden snapshots exist, delete `_build_navigation_context`'s `tool=None` branch in `bug_fix.py:624-692` and equivalents in `explain_repo` / `navigation_ctf`. `tool=None` becomes equivalent to `get_adapter("aethyme")`. Parity tests get repurposed per `parity-test-contract.md` "When divergence is acceptable" — they assert `adapter_output == golden_snapshot.json` instead of comparing live alternative implementations. Rationale for Option A: keeping the legacy path forces AethymeBench to import Aethyme's Python internals (`build_task_pack` / `build_task_context`), which defeats extraction. **Sequencing constraint: 2.16 → 2.3 → 2.2** (golden snapshots must exist before deletion or the canary dies in the gap). | 2 hours |
| 2.4 | **Build `eval_config.py` (or similar) as single source of truth for host-package settings** | Currently `AETHYME_PACKAGE_ROOT`, the engine binary path, etc. are scattered. One config file → easier to swap when extraction happens. Becomes `aethyme_bench.config` post-extraction. | 2 hours |
| 2.5 | **Implement the warm-cost measurement** | The Graphify-vs-Aethyme cost comparison silently hides Graphify's warm spend. Capturing warm-step cost separately (or counting it into total) closes the honest measurement gap. Strong methodology piece for extraction. | 4 hours |
| 2.6 | **Test the Aethyme manifest's `aethyme install` self-installer** (the equivalent of Graphify's `claude install`) | Aethyme needs its own per-clone register subcommand. Once present, the manifest can do `cd {{TARGET_REPO}} && aethyme install` symmetric with Graphify. | 2 hours |
| 2.7 | **Convert `aethyme-eval-ui` server's `sys.path.insert` to a proper import** | Currently the FastAPI server reaches into `packages/aethyme/src/eval/`. Convert to importing via pip-install path so it works in both in-tree and extracted mode. | 4 hours |
| 2.8 | **Make `repos.py:create_condition_repos` source-of-clones tool-aware** | Currently `source = target.aethyme_path` is wired in the orchestrator's prepare cli_cmd. For `tool=non-aethyme`, source should probably be `target.control_path` (tool gets installed via [register]). | 2 hours |
| 2.9 | **Move `src/contracts/` to a vendor-able location** | The ~200 LoC of contracts is needed by both eval and the (potentially extracted) framework. Either vendor or convert to a shared dep. | 1 hour |
| 2.10 | **Write the canonical methodology doc** (`docs/architecture/methodology.md`) | The condition matrix, mandatory `[notes].condition_mapping`, parity-test discipline, warm-cost surfacing, 4-section diagnostic report — these are the headline if you ever extract publicly. Pre-write the doc; extraction becomes "move package + reference this doc." | 4 hours |
| 2.11 | **Build a comprehensive cross-tool regression test** | One run that exercises bug-fix + explain-repo + navigation-ctf + dead-code (via prompts_writer with tool support post-2.1) with both `aethyme` and `graphify`. Locks the framework's correctness across both tools before extraction. | 1 day (+ ~$30 in eval spend) |

### New 2.12–2.16 (the five-axes plumbing + methodology snapshot)

| # | Item | Rationale | Estimate |
|---|---|---|---:|
| 2.12 | **Pluggable eval types — `EvalTypeAdapter` Protocol** | Today bug_fix / explain_repo / navigation_ctf / dead_code are wired into the orchestrator. Define a Protocol with `prepare(target, condition) -> EvalInputs`, `score(output, reference) -> ScoreReport`, and a registry. Existing eval types become first-class registered adapters; adopters can register their own. | 1 day |
| 2.13 | **Pluggable targets — config-loaded registry** | Convert `src/eval/targets.py` from a hardcoded dict into a loader that reads `targets/*.toml` (or YAML). AethymeBench ships an empty registry by default; Aethyme ships its `grc` and `mediawiki` targets via its own config. | 4 hours |
| 2.14 | **Pluggable backends — `BackendAdapter` Protocol** | Today `claude` and `codex` are wired via if/else into the orchestrator. Extract into a Protocol covering tab orchestration, prompt submission, output collection, JSONL parsing, pricing. Each backend becomes a registered adapter. | 1 day |
| 2.15 | **Pluggable scorers — `ScorerAdapter` Protocol** | Scoring logic lives inside prepare modules today. Extract into a Protocol so the same eval type can be scored multiple ways (e.g. quality-only vs. recall-only) and adopters can ship domain-specific scorers. | 6 hours |
| 2.16 | **Golden-file methodology snapshot + `methodology_hash`** | Compute `methodology_hash` at run-start (sha256 over prompts + scorers + condition matrix + manifest digests + version). Stamp into every run output's JSON. Write golden snapshot to `docs/methodology/snapshots/<hash>.json`. Implement `aethyme-bench methodology diff <a> <b>`. | 1 day |
| 2.17 | **Generalize the `negative-context` condition for non-Aethyme tools** (carried over from 1.8) | Today `bug_fix.py:602-606` hardcodes the preamble (`"Use Aethyme tools..."`) and `_NEGATIVE_NAV_CONTEXT_PATH`. Two sub-decisions: (a) does the preamble template via the same mechanism 2.1 uses for leverage, or via a tool-provided `[conditions.negative_context].preamble` block in the manifest? (b) when a tool has no meaningful negative-context test (the historical default for everything that wasn't Aethyme), should the condition be cleanly *skipped* per-tool (the methodologically honest choice) or *replayed-as-leverage* (the current behaviour, which silently double-counts)? Stake out a position and implement it; document the answer in `[notes].condition_mapping` for every tool. | 4 hours |

**Stage B total: ~7-8 days of focused work + one large regression run.**

**Critical-path items inside Stage B:**

- **2.16 → 2.3 → 2.2** (revised 2026-05-19): 2.3 was originally "decide the
  legacy-path question." The decision landed (Option A — remove the path);
  what remains is sequencing the deletion *after* 2.16 captures golden
  snapshots. Without snapshots, the parity tests have nothing to compare
  against once the legacy path is gone, and Stage 2.5's black-box install
  check has no reference. 2.2 (the `in_tree=true` reshape) then follows
  2.3 unchanged.
- **2.1 (prompts.py templating) gates 2.11 (comprehensive regression run).** Diagnostic evals can't run with non-aethyme tools until prompts.py is reshaped.
- **2.7 (UI import strategy) can be done in parallel with everything else.**
- **2.12-2.15 (plugin Protocols) should be done after 2.1-2.10** — refactoring on top of unfinished decoupling is double work.
- **2.16 (methodology_hash) can be done any time after 2.10** (canonical methodology doc), but **must precede 2.3** per the revised sequencing above.

---

## Stage 2.5 — Monorepo soak (NEW)

**Goal:** Stage the extraction inside the Aethyme monorepo as
`packages/aethyme-bench/` (or similar) before publishing externally.
Use the framework as a separate-import-path package for some weeks
before committing to a `git filter-repo`.

**Why this stage exists:** Stage C in the previous plan was "the
irreversible commit." Adding 2.5 makes it possible to *rehearse* Stage
C without paying its cost — and to discover boundary-violation issues
when you have time to fix them, not in a deadline-fixing scramble.

**Validation gate:** Aethyme's eval CI passes with all imports
rewritten to `from aethyme_bench import ...`. Zero references to
`src.eval` outside of `packages/aethyme-bench/`. Cross-tool regression
test (2.11) still passes against the in-monorepo package.

| # | Item | Rationale | Estimate |
|---|---|---|---:|
| 2.5.1 | Create `packages/aethyme-bench/` with proper `pyproject.toml`, MIT LICENSE, CalVer version | The structural shell, before any file moves | 1 hour |
| 2.5.2 | Move `src/eval/`, `src/eval/tools/`, `evals/tools/`, `src/scorecard/`, `packages/aethyme-eval-ui/` into the new subdir | Mechanical move; preserves uncommitted state because we're not yet using `git filter-repo` | 2 hours |
| 2.5.3 | Rewrite imports throughout Aethyme: `src.eval` → `aethyme_bench`, `src.scorecard` → `aethyme_bench.scorecard`, etc. | Mechanical sed pass + spot-check; `make audit-aethyme-references` should pass cleanly | 3 hours |
| 2.5.4 | Update Aethyme's `pyproject.toml` to depend on `aethyme-bench` via local workspace path (uv / hatch workspaces) | Aethyme installs aethyme-bench from `../aethyme-bench` rather than its own tree | 1 hour |
| 2.5.5 | Move tests: `tests/local/test_eval_*` and `tests/scorecard/*` into `packages/aethyme-bench/tests/` | The tests must travel with the code; they're part of the contract | 2 hours |
| 2.5.6 | Move docs: `docs/guides/eval-protocol.md`, `docs/architecture/methodology.md` into `packages/aethyme-bench/docs/` | User-facing methodology lives with the package | 1 hour |
| 2.5.7 | Add a stub `HISTORY.md` to `packages/aethyme-bench/` documenting pre-split development | Per Q40 — pre-split context preserved for adopters | 1 hour |
| 2.5.8 | Run **two full cross-tool regression rounds** (one from Aethyme, one from a fresh clone treating aethyme-bench as a black-box install) | The asymmetry test: does the package work from outside Aethyme? | 1 day (+ ~$60 spend) |
| 2.5.9 | **Soak period (calendar time, not effort): use the package for ≥2 weeks** in real eval work without modifications crossing the package boundary | Real-use surfaces the leaks no audit can. Touch only `aethyme-bench` files via `aethyme-bench`'s own commits. | 2-4 weeks calendar |

**Stage 2.5 total: ~1.5 days of focused work + ~$60 + 2-4 weeks calendar soak.**

**This is the new reversibility boundary.** After 2.5, the package is
ready for Stage C. Before 2.5, structural problems can still be fixed
in-place. The whole rationale for adding 2.5: turn Stage C from an
irreversible commit into a verifiable mechanical step.

---

## Stage C (Phase 3) — Decoupling

**Goal:** Move AethymeBench into its own repository. Aethyme depends
on the new package the same way any external tool would.

**Validation gate:** Aethyme's CI still runs all its evals via the (now
external) AethymeBench, with results byte-identical to pre-extraction
baseline runs.

| # | Item | Rationale | Estimate |
|---|---|---|---:|
| 3.1 | Create the new GitHub repo `aethymebench/aethymebench` | The structural shell. PyPI publication deferred per locked decision. | 1 hour |
| 3.2 | Use `git filter-repo` (or similar) to extract `packages/aethyme-bench/` *with history* | Preserves blame / commit context. Stage 2.5 made this rehearsable. | 2-4 hours |
| 3.3 | Finalize `HISTORY.md` documenting the pre-split development period | Per Q40 — the long-form version of the stub written in 2.5.7. References specific commits from the Aethyme repo's pre-split era. | 2 hours |
| 3.4 | Set up CI on the new repo | Probably mirror Aethyme's existing CI structure | 2 hours |
| 3.5 | Cut the first CalVer tag (e.g. `2026.06.01`) and publish a GitHub Release | Pin against this tag from Aethyme | 1 hour |
| 3.6 | Update Aethyme's `pyproject.toml` to depend on `aethymebench` via git URL pinned to the tagged commit | Aethyme's CI now installs AethymeBench from GitHub, not from local workspace | 1 hour |
| 3.7 | Delete `packages/aethyme-bench/` from the Aethyme monorepo | The monorepo subdir is now redundant; the canonical home is the new repo | 30 min |
| 3.8 | Smoke-test from the new repo against the playground | One full eval run via the now-external framework | 2 hours + ~$5 |
| 3.9 | Bidirectional verification: a known-good historical eval through the extracted framework yields byte-identical output | The parity-test contract, applied across the publication boundary | 2 hours + ~$5 |
| 3.10 | Write `docs/migration.md` in the new repo for adopters | "How to add your tool to this eval framework" — the manifest's `[notes]` field becomes the centerpiece, the `tool` variable becomes the iteration-speed affordance | 2 hours |
| 3.11 | Document Chau7 dependency explicitly in adopter docs | Per Q36 — adopters need to know AethymeBench is Chau7-orchestrated; cost is "an estimation" rather than billing-accurate. | 1 hour |

**Stage C total: ~2 days of focused work + ~$10 in verification spend.**

---

## Summary

| Stage | Effort | Reversibility | Value if you stop here |
|---|---|---|---|
| Stage A | ~5-6 hours | Fully reversible | Cleaner in-tree framework; no Aethyme-specific naming creep; better debuggability |
| Stage B | ~7-8 days + $30 | Mostly reversible | Framework is multi-tool with no Aethyme privilege; five plugin axes plumbed; methodology hash + golden snapshots in place; comprehensive regression test exists |
| Stage 2.5 | ~1.5 days + $60 + 2-4 wk calendar | Reversible (it's still in the monorepo) | AethymeBench is a real package, used as a real package, for a real soak period. The "would extraction work?" question is empirically answered. |
| Stage C | ~2 days + $10 | Irreversible (publication) | AethymeBench is its own repo, MIT-licensed, on GitHub, CalVer-tagged, with documented methodology and adopter migration path. |

**Cumulative cost:** ~11 days focused work + ~$100 eval spend +
2-4 weeks calendar (Stage 2.5 soak).

## Four scenarios for how this might unfold

1. **Stop after Stage A.** The cleanup is valuable on its own. The
   framework is more grep-able and less Aethyme-name-creep-laden.

2. **Stop after Stage B.** Aethyme has a fully tool-neutral framework
   in-tree, the methodology is documented, the five plugin axes are
   plumbed, methodology_hash + golden snapshots provide reproducibility
   guarantees, and the cross-tool regression test proves it works.

3. **Stop after Stage 2.5.** AethymeBench is a real-but-internal
   package. Aethyme uses it via workspace dependency. No external
   surface, no maintenance burden of a separate repo, but you have
   genuine package-boundary discipline.

4. **Do Stage C.** AethymeBench is its own MIT-licensed, GitHub-hosted
   package. The methodology doc + the `tool`-variable affordance + the
   structured `[notes]` audit trail are the value prop; the runner
   code is the carrier. Researchers and other tool developers can
   adopt it.

## Cross-references

- Initial extractability audit: chat history of session 2026-05-15 (3
  flavors A/B/C; the manifest system absorbed most of Flavor A's
  initial complexity)
- Planning round that locked all of the above: session 2026-05-18/19
  (43-question Q&A, see this commit's accompanying message for the
  decisions index)
- Tool-adapter manifest system architecture: commits `d27d931`,
  `0025b12`, `06edac0`, `d47a137`, `c9f773b`, `6997b55`, `c9e2dec`,
  `c64f98b` (2026-05-15 through 2026-05-18)
- Parity-test discipline: `tests/local/test_eval_navigation_context_adapter_parity.py`,
  `tests/local/test_eval_explain_repo_adapter_parity.py`,
  `tests/local/test_eval_navigation_ctf_adapter_parity.py`
- Manifest validation contract: `src/eval/tools/manifest.py`
- The `[notes].condition_mapping` mandatory-field rule: same file
- Graphify manifest as exemplar of structured `[notes]` audit trail:
  `evals/tools/graphify.toml`
