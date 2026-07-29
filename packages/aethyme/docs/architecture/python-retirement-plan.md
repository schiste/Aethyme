# Python Retirement Plan — one binary, one runtime

Last Updated: 2026-07-17

Status: PROPOSED. Owner: operator. Prereq reading:
`cross-process-consumers.md`, `graph-schema.md`, the redb migration plans.

## Objective

Retire `packages/aethyme/src/` (the Python workflow layer, ~10k lines) by
absorbing its surface into the Rust workspace — and use the move to fix
the design debts a 1:1 port would preserve. End state: `cargo install`
yields the **entire** product; no venv, no root discovery, no transport
adapter, no second test stack. `packages/aethyme-eval` deliberately stays
Python (arm's-length acceptance harness — see Non-goals).

This is the missing item behind the V1 exit criterion ("a stranger
installs in <15 min with one binary"): today `cargo install` covers only
`explore`/`broker`/`certify`; everything else needs Python ≥3.11, an
editable pip install, and the `aethyme root` pointer machinery that
exists solely to locate that venv.

## What the Python package actually is (2026-07-17 inventory)

| Module | Lines | Nature | Port class |
|---|---|---|---|
| `cli.py` | 2,304 | Click groups; ~50 commands | ~35 are **thin engine delegation** (query/graph/task/facts/analyze/repo basics) — mechanical. ~15 carry logic (below). |
| `indexing/engine.py` | ~700 | Transport adapter (subprocess/PyO3), binary build, snapshot-keyed cache | **Deleted**, not ported — native calls need no bridge. Cache semantics move into the engine (see Improvements #5). |
| `indexing/` others | ~2,200 | skills compile, repository_snapshot, commit_hygiene, experience_telemetry, onboarding | Port (real logic, mostly text/JSON) |
| `scorecard/` | 1,672 | 8 heuristic detectors + scoring engine + pydantic models | Port into new **quality** domain |
| `autofixers/` | 1,866 | 5 patch-based fixers + safety/risk engine + git/PR helper | Port into same **quality** domain |
| `enhance.py` | 751 | Skill/AGENTS.md deployment, override rendering, broker protocol section | Port (templating; determinism-sensitive) |
| `contracts/`, `rendering/` | ~500 | Versioned schemas, text rendering | Collapse into Rust (single-source the contracts) |
| `api/ auth/ graph/ middleware/ models/` | ~0 | pycache husks + 5-line `models/__init__` | Delete |

Dependency reality check: declared deps include `tree-sitter`, `watchdog`,
`httpx`, `prometheus-client`, `psutil`, `python-multipart` — **all dead**
(Gen-0 leftovers). Live surface: `click`, `pydantic` (2 files), stdlib.
The package is already hollowed out; this plan finishes the job.

## Target architecture

```
rust/crates/
  aethyme-cli        NEW — owns the `aethyme` binary (moved from aethyme-engine).
                     All command groups native. Thin: parse → library call → render.
  aethyme-quality    NEW — the scorecard+autofix unification (see Improvements #1).
                     detect/ (8 detectors) · fix/ (5 fixers) · safety/ · report/
  aethyme-enhance    NEW — skill compile+deploy, AGENTS.md rendering, overrides,
                     onboarding, commit hygiene, experience telemetry.
  aethyme-engine     unchanged role; loses the router binary; gains output-cache
  aethyme-broker     unchanged
  aethyme-graph-*    unchanged
```

Deleted at the end (not ported): `src/` entirely, `pyproject.toml`,
`requirements-dev.txt`, the `.venv` bootstrap, `aethyme root`
env→pointer-file→upward-walk discovery, the PyO3 `aethyme_py` transport,
the Python-daemon socket namespace remnant, `check-cross-process-contract.py`'s
Python-invocation rows.

## Improvements (why this is a refactor, not a port)

1. **Unify scorecard + autofixers into one quality domain.** Today they
   are parallel hierarchies with duplicated file-walking, separate
   finding shapes, and a scorecard→fixer mapping that lives in
   `normalize_fixes` glue. Redesign: one `Finding` model (detector emits
   it, fixer consumes it, report renders it), one repo-walk, safety
   levels expressed on the finding itself. `ai-ready` and `autofix`
   become two verbs over one pipeline.
2. **Single-source the contracts.** Schema versions currently exist in
   Python (`contracts/versions.py`) and Rust (json views) with a sync
   test holding them together. Serde types in one crate become the only
   definition; the sync test dies with the duplication.
3. **Deterministic enhance.** `enhance deploy` output should be
   byte-identical given identical inputs (same discipline as
   certify/scaffold, which tests already enforce). Golden-file tests on
   rendered AGENTS.md/skills replace the current partial checks.
   **RESOLVED 2026-07-30 — deterministic except `generated_at`, by
   design.** The onboarding artifact's `freshness` block carries the
   deterministic staleness signals (`snapshot_key`, `commit`,
   `repo_dirty`); `generated_at` is the human-facing wall-clock stamp
   and stays `now_iso_utc()` — replacing it with e.g. the commit
   timestamp would lie on dirty repos and add nothing the snapshot key
   doesn't already provide. Parity/golden tooling scrubs the timestamp
   (enhance-golden.sh already does); everything else renders
   byte-identical given identical inputs.
4. **One config surface.** Scattered env vars (`AETHYME_CACHE_DIR`,
   `AETHYME_ENGINE_TRANSPORT`, `AETHYME_REQUIRE_LOCAL_ENGINE`, …)
   consolidate into `.aethyme/config.toml` + a documented env override
   table. Transport vars die outright.
5. **Move the output cache into the engine.** ~~The snapshot-keyed cache
   in `/tmp/aethyme-cache` is engine-adjacent logic stranded in Python.~~
   **RESOLVED 2026-07-28 — no engine cache.** Measured post-flip on the
   medium fixture (10 files) and the Aethyme worktree (388 source
   files), 3-run averages, release build: redb-backed commands (query/
   graph/task) are ~19–23ms flat regardless of repo size — pure process
   startup; a cache cannot help. Map-building commands (facts, analyze,
   repo inspect) scale with repo size (20ms → 54–66ms at 388 files) and
   are the only class the old Python output cache amortized. Decision:
   the redb store IS the cache — a materialized, explicitly-refreshed
   artifact. Rather than cache around per-invocation map builds, the
   remaining map-based surfaces follow the redb workstream's existing
   trajectory (facts/analyze redb-backing), which removes the cost
   instead of hiding it. `/tmp/aethyme-cache` and its `clear-cache`
   reinterpretation are legacy stubs that die in Phase 6.
   **Scale check (2026-07-28, 4,851-file synthetic repo, 9,450 resolved
   edges):** redb reads stay near-flat (25–31ms; task anchors 85ms);
   fragment-based map builds are cheap even here (facts 106ms, inspect
   130ms) — but `analyze dead-code` is **4.4s**, and the cost is the
   analyzer's per-function caller scans, not map building. So the one
   place the old Python cache is genuinely missed is repeated identical
   analyze runs at playground scale — an eval-harness pattern, not an
   operator pattern. The structural fix is redb-backing the analyzer
   (with the redb owners), not resurrecting a cache for one command.
6. **Typed errors + exit-code contract.** Click's ad-hoc `SystemExit(1)`
   becomes the documented exit-code table the router already started
   (explore's exit-2-daemon-down convention).
7. **Latency.** Every delegated command currently pays Python startup +
   subprocess hop (~150–300ms floor). Native commands pay ~0. This is
   the same win that motivated moving `explore` in-process (#31).

## Non-goals

- **`packages/aethyme-eval` stays Python.** It is the acceptance harness;
  keeping it in a different runtime at arm's length is a feature
  (Cardinal Rule 1/2 hygiene), and it is user-led.
- **No graph-backed detector upgrades inside this migration.** Detectors
  port behavior-first (same findings on same fixtures). Re-basing
  route/ability coverage on the redb graph is a V2 follow-up with its
  own acceptance via aethyme-eval — mixing it in would make parity
  unverifiable.
- **No eval-shaped changes.** Nothing here may special-case eval
  scenarios (Cardinal Rule 2). The parity harness runs on fixtures and
  playgrounds as *diagnostics*, not targets.

## Migration mechanics

**Strangler via the router.** The `aethyme` binary already dispatches
per-command. Each phase moves a command group to native dispatch and
deletes its Python counterpart in the same change — the CLI surface
(names, flags, JSON shapes) is frozen; only the implementation moves.
No long-lived dual implementation: per group, the flip is one commit.

**Parity harness (the gate for every phase).**
1. *Pre-step (Phase 0):* convert `tests/local`'s CliRunner-import tests
   to invoke the `aethyme` binary as a subprocess. Same assertions then
   verify whichever implementation answers — the suite becomes
   implementation-blind and gates the whole migration unchanged.
2. *Golden diff:* a script runs the frozen command list against a
   fixture repo + a playground pair on both implementations and diffs
   canonicalized output (timestamps/paths normalized). Byte-parity is
   the bar everywhere — delegation commands and quality commands, JSON
   and markdown alike (decision #2). Where pydantic float/ordering
   quirks make Python's bytes awkward to replicate, the Rust side
   replicates them anyway; cosmetic cleanup is a post-migration change
   with its own diff review.
3. Existing pytest suites for scorecard/autofixers translate to Rust
   tests in their phase; until then they keep running against the
   Python side, which still exists for exactly the unported groups.

**Cross-process protocol (Cardinal Rule 3).** Every phase greps
`cross-process-consumers.md` and updates rows in the same commit. Known
hot spots: `skills/aethyme/aethyme-load-context.sh:48` invokes
`python -m src.cli repo record-wrapper-invocation` (flips to `aethyme
repo record-wrapper-invocation` in Phase 3); deployed skill templates
reference `python -m src.cli` (re-render + redeploy in Phase 2;
`verify-playground.sh` freshness greps updated the same commit);
CI workflows and `Makefile`/`package.json` pytest entries shrink per
phase. Contract decision per phase: `soft-retire` of the Python
invocation spelling, `hard-delete` only in Phase 6.

**Broker-driven, in worktree sessions.** Each phase is a broker
*worktree* session, never the shared main checkout (Phase 0 lesson,
2026-07-27: concurrent sessions' uncommitted edits in the shared tree
were absorbed into a migration commit, and one migration edit was
clobbered by a concurrent save). Gates on the merged tree are the
enforcement (the redb-fixture incident of 2026-07-17 shows why:
environment-dependent skips are a known blind spot — the parity
harness must not skip silently when the engine is absent; it fails).
Since 2026-07-27 the gates include `cross-process-contract`, so every
entry that removes tracked symbols must carry a
`Contract decision: <label>` line in a commit message.

## Phases — overview

| # | Scope | Deletes | Est. |
|---|---|---|---|
| 0 | Seams: tests→subprocess, golden harness, surface freeze, registry audit | dead pyproject deps | S |
| 1 | Delegation groups native (query/graph/task/facts/analyze/repo basics/intents) | ~60% of `cli.py`, most of `engine.py` | M |
| 2 | `aethyme-enhance`: deploy/verify, skills compile, AGENTS rendering | `enhance.py`, `indexing/skills.py` | M |
| 3 | Repo-UX helpers: onboarding, hygiene, telemetry, wrapper hook | rest of `indexing/` | M |
| 4 | `aethyme-quality` detect side: 8 detectors + scoring + reports | `scorecard/` | L |
| 5 | `aethyme-quality` fix side: 5 fixers + safety + patch + PR helper | `autofixers/` | L |
| 6 | Retirement sweep: delete `src/`, tooling, registry rows, docs | everything remaining | M |

S ≈ a session, M ≈ 2–4 sessions, L ≈ a week-scale effort. After 0,
phases 1–3 are independent of 4–5; 4 must precede 5 (fixers consume
findings). The mechanical 60% is cheap; **the real port is scorecard +
autofixers (~3.5k lines of heuristics with a redesign)** — budget most
of the calendar there.

## Phase 0 — Seams (make the migration verifiable before moving anything)

**Goal.** After this phase, every later phase can prove parity
mechanically. No behavior changes.

**Work items.**
1. *Implementation-blind tests.* `tests/local/` currently imports
   `src.cli` and drives Click's `CliRunner` in-process
   (`test_local_workflow.py`, `test_cli_completeness_signals.py`,
   `test_enhance.py`, …). Convert these to invoke the `aethyme` binary
   as a subprocess and assert on stdout/exit codes. Exception:
   `test_engine_cache.py` tests `engine.py` *internals* (transport
   registry, cache identity) — it is implementation-specific by nature
   and retires with `engine.py` in Phase 1; mark it so now.
2. *Golden-diff harness.* `scripts/migration/golden-diff.sh`: runs a
   frozen command list against (a) a committed fixture repo built by
   `tests/support/repo_builders.py` logic and (b) a playground pair,
   through both implementations, normalizes volatile fields
   (timestamps, absolute paths, durations), and diffs. Must **fail
   loudly, not skip**, when the engine binary is missing — direct
   lesson from the 2026-07-17 gate incident where an engine-skip
   masked a real break.
3. *Command-surface freeze.* Generate `docs/architecture/cli-surface-v1.md`
   from `--help` output: every command, flag, env var, exit code, and
   JSON top-level shape. This is the contract each phase must hold;
   `docs/reference/cli.md` is checked against it.
4. *Registry re-audit.* Grep-verify every `cross-process-consumers.md`
   row that mentions `python`, `src.cli`, or `.venv` (the 2026-07-17
   audit found the registry drifts in both directions).
5. *Dependency purge.* Drop the dead pyproject deps (`tree-sitter`,
   `watchdog`, `httpx`, `prometheus-client`, `psutil`,
   `python-multipart`) so later phases see the true surface.

**Exit criteria.** tests/local green while invoking the binary path
(Python still answering); golden harness produces a clean both-sides
run on the fixture; surface-freeze doc committed; registry audit clean.

**Risks to manage.**
- *Subprocess conversion changes what's tested* — CliRunner captures
  Click internals (exceptions as exit codes) differently from a real
  process. Convert one file per commit and watch for assertions that
  silently weaken (e.g. `result.exception` checks have no subprocess
  equivalent — rewrite them as stderr/exit-code assertions).
- *Fixture too small to be representative* — the demo repo is 3 files.
  Add a medium fixture (multi-language, unicode paths, symlinks) now;
  discovering fixture gaps in Phase 4 is far more expensive.
- *Startup-latency tax in tests* — ~50 subprocess invocations replace
  in-process calls; keep suite wall-time in check by batching repo
  setup per module, not per test.

## Phase 1 — Delegation groups go native

**Goal.** The ~35 thin commands answer natively: `query
symbol|deps|impact`, `graph node|children|parents|callers|callees|docs|
configs|expand|overview`, `task pack|context|anchors|scope|next|expand|
explain`, `facts public-functions|function-usage`, `analyze dead-code`,
`repo ingest|inspect|clear-cache|warm|engine-info`, `intents`.

**Work items.**
1. Stand up the `aethyme-cli` crate (clap) and move the router binary
   into it; `aethyme-engine` becomes a pure library dependency here.
2. Per group: implement native dispatch calling the engine library
   directly (the redb views and map APIs the engine CLI already uses),
   port the text renderers from `rendering/context_pack.py`
   (`render_pack_summary`, `render_explain_repo_text`), delete the
   Python command + its `engine.py` wrapper in the same commit.
3. Move output caching engine-side (Improvements #5): the snapshot
   identity logic from `indexing/repository_snapshot.py` (commit +
   dirty-state key) becomes an engine concern; measure first — post-redb,
   many commands may not need a cache at all. Delete
   `/tmp/aethyme-cache` handling from Python as groups move.
4. `repo engine-info` is reinterpreted: transports are gone, so it
   reports binary path/version/store status. Update `cli-surface-v1.md`
   with an explicit, documented exception (the one intentional
   surface change of this phase).
5. Registry: engine.py row shrinks per group; final commit of the phase
   rewrites it.

**Exit criteria.** Golden diff byte-parity for every migrated command
on fixture + playground; tests/local green natively; `cli.py` contains
only enhance/repo-UX/ai-ready/autofix/intents-data; no command pays
Python startup.

**Risks to manage.**
- *Click semantics don't map 1:1 to clap* — env-var-backed options
  (`AETHYME_TENANT_ID`), `--json` vs `--json-output` inconsistencies,
  Click's prefix matching. The freeze doc pins the exact behavior;
  where Click was accidentally lax, replicate the lax behavior (parity
  first, tidy in Phase 6 with a documented deprecation).
- *Cache-semantics drift* — Python cached some command outputs; native
  calls may return fresher data (e.g. after fragment changes without
  re-index). That is a *behavior* change masked as a speedup. Decide
  cache policy per command before flipping it, and encode it in the
  golden harness (run twice, dirty the repo between).
- *Active collision with redb sessions* — this phase edits
  `aethyme-engine-cli.rs` and navigation code the redb phase-set is
  actively rewriting. Per decision #4: run concurrently, lease-
  coordinated per file via the broker — adopt before editing, watch
  `broker status` overlaps, negotiate rather than block.
- *PyO3 binding* — per decision #3, retired **in this phase**: delete
  `aethyme_py`, the pyo3 transport arm, and `--engine-transport`
  plumbing in the same commits that remove their `engine.py` consumer.

## Phase 2 — `aethyme-enhance` (deployment & templating)

**Goal.** `enhance deploy|verify`, `repo compile-skills|deploy-skills`
native, with byte-deterministic outputs.

**Work items.**
1. New crate `aethyme-enhance`: port the `EnhancementTarget` table,
   `{{AETHYME_ROOT}}` substitution, `AETHYME:BEGIN/END` generated-block
   splicing, `.claude/settings.local.json` merge, the broker-protocol
   and repo-routing AGENTS.md sections, and agents-override rendering
   (`.aethyme/overrides/agents.json`) from `enhance.py`; skill
   compilation from `indexing/skills.py`.
2. Golden-file tests: rendered AGENTS.md/CLAUDE.md/skill files
   byte-compared against committed goldens for fixture repos with and
   without overrides.
3. Flip the deployed-template *content*: generated skills and AGENTS
   sections stop saying `python -m src.cli …` and say `aethyme …`.
   Same commit: update `verify-playground.sh` freshness greps and the
   registry rows for `skills/aethyme/SKILL.md` (this is the protocol
   the 2026-05-08 playground breakage taught — Class 3 change).
4. Redeploy to active playground pairs; run `verify-playground.sh`
   against each.

**Exit criteria.** Goldens byte-identical across two consecutive runs
and across machines (macOS/Linux CI); playground redeploy + verify
green; no deployed artifact references the Python spelling.

**Risks to manage.**
- *Stale deployed skills in the wild* — every previously-enhanced repo
  still carries `python -m src.cli` templates until re-enhanced. The
  old spelling must keep working until Phase 6 (soft-retire), and
  `enhance verify` should detect and report the stale spelling so
  operators re-deploy deliberately.
- *JSON merge non-determinism* — `settings.local.json` merging must
  produce stable key order (serde_json preserves insertion order;
  define canonical ordering explicitly).
- *Overrides are user data* — `.aethyme/overrides/agents.json` files
  exist in real repos with real content; the Rust parser must accept
  everything the Python `_load_agents_overrides` accepted (including
  its tolerances for missing keys), verified against a corpus of
  override files, not just the template.

## Phase 3 — Repo-UX helpers

**Goal.** The remaining `repo` subcommands native: onboarding + agents
override init/validate, `commit-message-template`, `lint-commit-message`,
`experience-telemetry`, `experience-status`, `record-wrapper-invocation`.

**Work items.**
1. Port `indexing/onboarding.py`, `commit_hygiene.py`,
   `experience_telemetry.py` into `aethyme-enhance` (they are
   deployment-adjacent UX, not engine concerns).
2. Flip the deployed hook: `skills/aethyme/aethyme-load-context.sh:48`
   invokes `python -m src.cli repo record-wrapper-invocation`. The
   template flip happened in Phase 2; this phase confirms the native
   command accepts identical args and the *old* spelling still resolves
   (both implementations answer during the transition).
3. Telemetry file compatibility: `experience-telemetry` reads repo-local
   artifacts written by prior versions — the Rust reader must parse the
   existing on-disk format; add a versioned header on write so future
   changes are detectable.

**Exit criteria.** Hook fires end-to-end natively in a playground
(wrapper invocation recorded, telemetry readable); commit-hygiene
lint produces identical verdicts on a corpus of real commit messages
from this repo's history.

**Risks to manage.**
- *The hook is fire-and-forget* — `record-wrapper-invocation` failures
  are invisible at call sites inside deployed repos. Instrument the
  native command to log to the repo-local telemetry file on arg-parse
  failure rather than dying silently, and test the hook path in
  verify-playground.
- *Telemetry format archaeology* — old artifacts may have shapes the
  current Python tolerates by accident. Collect real files from the
  dogfood repos into the fixture corpus before porting the reader.

## Phase 4 — `aethyme-quality`, detect side

**Goal.** `ai-ready` native: the 8 detectors (`ability_coverage`,
`data_ui_coverage`, `folder_docs`, `generated_files`, `i18n_gaps`,
`relative_links`, `route_coverage`, `schema_drift`), the scoring engine
(blocker/warning/info weights), and json/md/both report rendering — on
the new unified `Finding` model.

**Work items.**
1. Crate skeleton first: `Finding` (id, detector, severity, file/span,
   message, fix-hint), `Detector` trait, one shared repo walk with
   ignore rules, scoring from `scorecard/models.py` (the 100-point
   blocker×20/warning×5/info×1 formula), report renderers.
2. Port detectors **one per commit**, each with: its pytest cases
   translated to Rust tests, plus a fixture-corpus diff run
   (old vs new findings as canonicalized JSON).
3. Pydantic models → serde (only 2 files use pydantic; `schema_drift`'s
   model introspection needs a design pass — it currently reflects over
   pydantic constructs).
4. Build the parity corpus deliberately: this repo, one playground,
   plus synthetic edge-case fixtures (unicode filenames, deep nesting,
   generated-file markers, malformed JSON schemas).

**Exit criteria.** Byte parity on the corpus (per decision #2): identical
finding sets per detector (file, span, severity), identical scores, and
`--format json` **and** `--format md` outputs byte-identical to the
Python renderer after volatile-field normalization; two consecutive
runs and macOS/Linux byte-identical; `tests/scorecard/` retired in
favor of the Rust tests.

**Risks to manage.**
- *Regex dialect gap* — Python `re` supports lookaround/backreferences;
  the Rust `regex` crate does not. Audit every detector pattern up
  front; where lookaround is load-bearing, either rewrite the pattern
  or take `fancy-regex` for that detector. Do the audit in the crate-
  skeleton commit, not mid-port.
- *Silent semantic drift* — same regex, different match set (unicode
  classes, `.` vs newline, path separators). The corpus diff is the
  only defense; a detector without corpus coverage does not flip.
- *Scoring float/rounding differences* — keep scores integer end-to-end
  as the Python formula already is; forbid f64 in the scoring path.
- *Scope temptation* — this is where "make detectors graph-backed"
  pressure appears. Resist: parity first (see Non-goals); file V2
  issues per detector instead.

## Phase 5 — `aethyme-quality`, fix side

**Goal.** `autofix` native: the 5 fixers (`docs_regenerator`,
`format_fixer`, `i18n_scaffolder`, `link_fixer`, `selector_inserter`),
the safety/risk engine, patch generation/application, and the git/PR
helper — consuming Phase 4's `Finding`s.

**Work items.**
1. Port `patch.py` (FilePatch, PatchGenerator, unified-diff rendering,
   dry-run/apply/requires-approval flow) and `safety.py` (risk levels,
   protected-path rules, validation) as the crate's spine.
2. Port fixers one per commit with corpus diffs of *produced patches*
   (byte-compared unified diffs), not just applied results.
3. `format_fixer` shells out to external tools (black, prettier,
   rustfmt, eslint) — keep that as subprocess orchestration; the
   fixer's own logic is tool discovery + orchestration, which ports
   cleanly.
4. `github.py` PR helper: port branch/commit/push orchestration;
   PR-mode stays gated behind explicit approval exactly as today
   (`requires_approval` on medium/high risk).
5. Wire detect→fix: `autofix` runs detectors from Phase 4 and maps
   findings to fixers natively, deleting the `normalize_fixes` glue.

**Exit criteria.** Fixture-corpus patch outputs byte-identical per
fixer; dry-run/apply/PR e2e covered by Rust tests including the
approval gate; `tests/autofixers/` retired.

**Risks to manage.**
- *This code mutates user repos* — the port must preserve the safety
  engine's exact protected-path and risk-escalation behavior before
  anything else; port and test `safety.py` first, fixers second.
- *External-tool variance* — formatter versions differ per machine;
  corpus tests must pin tool versions or assert on the orchestration
  (which tool, which args) rather than formatted bytes.
- *Patch-application ordering* — Python applies patches in list order
  with per-file failure tolerance (partial status). Replicate the
  partial-failure semantics; an all-or-nothing "improvement" here
  changes observable behavior and belongs in a follow-up, not the port.
- *PR mode touches remotes* — keep it opt-in and approval-gated;
  e2e tests run against a local bare remote, never a real one.

## Phase 6 — Retirement sweep

**Goal.** Python is gone from the product. `cargo install` is the whole
story.

**Work items.**
1. Delete `src/` (including the `models/` husk), `pyproject.toml`,
   `requirements-dev.txt`, venv bootstrap docs, `aethyme root`
   discovery (env → pointer file → upward walk) from the router, and
   the Python-daemon socket namespace remnant in `daemon.rs`. (PyO3
   transport already retired in Phase 1 per decision #3.)
2. Registry hard-delete ceremony: every row mentioning `python -m
   src.cli`, `engine.py`, or `.venv` — with the PR-body contract
   decision the checker enforces.
3. Contract-checker update: `scripts/check-cross-process-contract.py`
   itself is Python — port it to a `broker`/`certify` check or a Rust
   xtask (it is CI-load-bearing: `cross-process-contract.yml`).
4. Docs sweep: CONTRIBUTING, quickstart, tests/README, cli.md,
   how-it-works — the "no database or services" story becomes "no
   Python".
5. Test-stack endgame: `tests/local` (already subprocess-driven) either
   ports to Rust integration tests or remains a **dev-only** pytest
   harness. Note: `packages/aethyme-eval` keeps Python as a dev
   dependency regardless — the end state is "users need no Python";
   developers may.
6. CI: `aethyme-local-tests.yml` and `oss-ci.yml` drop Python setup
   for the product path; a clean-machine job proves the
   `cargo install` → full-suite → verify-playground chain.

**Exit criteria.** Fresh macOS and Linux machines: `cargo install`,
then the full local suite + verify-playground + a playground enhance/
deploy round-trip pass with no Python on PATH (for the product path).
Registry contains zero Python invocations.

**Risks to manage.**
- *Operator muscle memory and old notes* — `python -m src.cli` stops
  working. Ship a shim error for one release: a tiny `src/cli.py`
  tombstone that prints "use `aethyme …`" and exits 2? No — `src/` is
  gone; instead document the flip loudly in README + AGENTS.md and
  accept the hard break (announce, don't assume).
- *The contract checker porting itself* — do item 3 *before* deleting
  `src/`, or CI loses its guard exactly when the biggest deletion
  lands.
- *Long-tail artifacts* — `.pth`/egg-info/pycache husks on dev
  machines make deleted modules superficially importable (seen in the
  2026-07-17 audit: stale `__pycache__` masked missing sources).
  Include a `make clean`-equivalent sweep in the phase.

## Cross-phase risks

- **Concurrent redb sessions** — phases 1–3 touch files the redb
  phase-set is rewriting. Per decision #4, run concurrently and
  coordinate via broker leases: adopt before editing, check
  `broker status` overlaps on the shared files, keep migration commits
  small so conflicts stay cheap to resolve. The merged-tree gates are
  the backstop; a `.aethyme/broker-action-required.md` on submit is the
  expected resolution path, not an exception.
- **Gate blind spot: environment-dependent skips** — any parity or
  local test that skips without a built engine reports green while
  verifying nothing (proven live 2026-07-17). Harness and converted
  tests must fail, not skip, when the engine is absent; CI must build
  the engine before the suite.
- **Hidden consumers of the Python spelling** — the registry protocol
  is the defense; it drifts (audit finding), hence the Phase 0
  re-audit and per-phase row updates in the same commit as each flip.
- **Two implementations in flight** — bounded by per-group atomic
  flips: at any moment each command has exactly one implementation;
  only *unported* groups remain Python.
- **Windows** — out of scope (built-as-if-public says macOS/Linux),
  but single-binary removes the biggest future Windows blocker (venv).

## Decisions (operator, 2026-07-17)

Formerly open questions — all resolved; the phases above reflect them.

1. **One crate.** `aethyme-quality` holds both detect and fix sides —
   the unification is the point.
2. **Byte stability for `ai-ready --format md`.** Markdown reports are
   held to the same bar as JSON: byte-identical to the Python output on
   the parity corpus, and stable across runs/machines. Cost accepted:
   the Rust renderer replicates Python's exact formatting (whitespace,
   ordering, number rendering) before any cosmetic improvement.
3. **Retire PyO3 at Phase 1.** The `aethyme_py` binding and the
   `--engine-transport` machinery are deleted when their only consumer
   (`engine.py`) goes; nothing stays published for third parties.
4. **Coordinate with redb sessions via broker leases**, not by waiting.
   Migration phases 1–3 may run concurrently with V2 graph work; each
   migration session adopts through the broker, watches lease overlaps
   on the shared files (`aethyme-engine-cli.rs`, navigation), and
   negotiates per-file rather than blocking on the redb set landing.
   The merged-tree gates remain the backstop.
