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
4. **One config surface.** Scattered env vars (`AETHYME_CACHE_DIR`,
   `AETHYME_ENGINE_TRANSPORT`, `AETHYME_REQUIRE_LOCAL_ENGINE`, …)
   consolidate into `.aethyme/config.toml` + a documented env override
   table. Transport vars die outright.
5. **Move the output cache into the engine.** The snapshot-keyed cache in
   `/tmp/aethyme-cache` is engine-adjacent logic stranded in Python.
   Native commands hit the engine library directly; caching (where still
   needed post-redb — redb reads are already fast) becomes an engine
   concern with one implementation.
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
   canonicalized JSON (timestamps/paths normalized). Byte-parity is the
   bar for delegation commands; documented-diff parity for quality
   commands (where pydantic float/ordering quirks may differ).
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

**Broker-driven.** Each phase is a broker session; gates on the merged
tree are the enforcement (the redb-fixture incident of 2026-07-17 shows
why: environment-dependent skips are a known blind spot — the parity
harness must not skip silently when the engine is absent; it fails).

## Phases

| # | Scope | Deletes | Exit criterion | Est. |
|---|---|---|---|---|
| 0 | Seams: tests→subprocess, golden-diff harness, freeze command surface doc, registry audit, purge dead Python deps | dead pyproject deps | local suite green invoking the binary path against Python impl; golden harness runs both-sides on fixture | S |
| 1 | Delegation groups native: `query`, `graph`, `task`, `facts`, `analyze`, `repo` basics (ingest/inspect/clear-cache/warm/engine-info), `intents` | ~60% of `cli.py`, most of `engine.py` | byte-parity golden diff; tests/local green on native | M |
| 2 | `aethyme-enhance` crate: enhance deploy/verify, skills compile/deploy, AGENTS rendering + overrides | `enhance.py`, `indexing/skills.py` | golden AGENTS.md/skills byte-identical; playground redeploy + verify-playground green; templates say `aethyme`, not `python -m src.cli` | M |
| 3 | Repo-UX helpers: onboarding + agents overrides, commit hygiene, experience telemetry, record-wrapper-invocation | rest of `indexing/` except snapshot | hook flipped; telemetry JSON parity | M |
| 4 | `aethyme-quality` detect side: 8 detectors + scoring + `ai-ready` reports (json/md) | `scorecard/` | same findings on fixture corpus (documented-diff parity); scorecard pytest suite retired for Rust tests | L |
| 5 | `aethyme-quality` fix side: 5 fixers + safety + patch + git/PR helper on the unified Finding model | `autofixers/` | fixer outputs byte-identical on fixture corpus; dry-run/apply/PR e2e in Rust tests | L |
| 6 | Retirement: delete `src/`, pyproject, venv bootstrap, root discovery, PyO3 transport, Python rows in registry + contract checker; docs sweep (CONTRIBUTING/quickstart/tests-README again); `aethyme-cli` crate owns the binary | everything remaining | `cargo install` on a clean machine passes the full local suite + verify-playground; no `python` invocation anywhere in registry | M |

S ≈ a session, M ≈ 2–4 sessions, L ≈ a week-scale effort. Phases 1–3 are
independent of 4–5 after 0; 4 before 5 (fixers consume findings).
Total honest estimate: the mechanical 60% is cheap; **the real port is
`scorecard` + `autofixers` (~3.5k lines of heuristics with a redesign)**
— budget most of the calendar there.

## Risks

- **Behavioral drift in heuristics** (regex semantics, path handling,
  unicode) — mitigated by fixture-corpus parity diffs before the Python
  side is deleted, and by porting detector-by-detector.
- **Hidden consumers of the Python spelling** — the registry protocol is
  the defense; the 2026-07-17 audit found it drifts, so Phase 0 includes
  a registry re-audit. Anything calling `python -m src.cli` outside the
  repo (operator muscle memory, old notes) breaks at Phase 6 —
  announce, don't assume.
- **Two implementations during phases 4–5** — bounded: per-group flips
  are atomic; only *unported* groups still exist in Python at any time.
- **Windows** — out of scope (built-as-if-public says macOS/Linux), but
  going single-binary removes the biggest future Windows blocker (venv).

## Open questions for the operator

1. Crate naming/split: `aethyme-quality` as one crate (recommended — the
   unification is the point) vs separate scorecard/autofix crates?
2. Does `ai-ready --format md` output need byte-stability for any
   downstream consumer, or is documented-diff parity acceptable?
3. Retire the PyO3 `aethyme_py` binding at Phase 1 (its only consumer is
   `engine.py`) or keep it published for third-party Python users until
   Phase 6? No in-repo consumer will remain after Phase 1.
4. Sequencing against V2 graph work: phases 1–3 touch files the redb
   sessions are actively changing — coordinate via broker leases, or
   schedule after the redb phase-set lands?
