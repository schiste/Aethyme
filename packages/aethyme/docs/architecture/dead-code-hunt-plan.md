# Dead-Code Hunt Plan — standing protocol

Last Updated: 2026-08-07

Status: ACTIVE. Companion to `python-retirement-plan.md` and
`../guides/dead-code-baseline.md`. Born from the 2026-07-17 audit
(~3.9k lines removed) and two near-misses that shaped the protocol
below.

## Why a standing plan instead of another sweep

The audit's root cause was structural: dead code accumulated because
nothing watched the seams. The seams are now guarded (contract gate,
implementation-blind suites, verify-playground), so the remaining work is (a) a
small known inventory with natural deadlines, (b) trigger-driven
cleanup as two large migrations land, and (c) a repeatable protocol so
future hunts don't re-learn the same lessons.

## The two near-misses the protocol encodes

1. **`verify-playground.sh` (2026-07-17)** — flagged as an orphan by
   reference-counting; actually a load-bearing safeguard whose *target
   registry* was broken. The fix was repair, not deletion.
2. **The four "orphaned" engine subcommands (2026-07-28)** —
   `activate`, `activate-from`, `task-localize`, `explain` showed zero
   consumers under `grep "\"$cmd\""` … because the consumers registry
   writes them as `` `activate` `` (backticks), not `"activate"`. They
   are documented redb V2 contract surfaces with dedicated test gates,
   ported to redb *this month*. A quoted-string grep pattern nearly
   deleted an actively-invested API.

## Liveness protocol (run per candidate, in order — stop at first LIVE)

A symbol/command/file is LIVE if ANY of:

1. **In-repo callers** — grep across `src/`, `rust/`, `tests/`,
   `scripts/`, `skills/`, `.github/`, `Makefile`, `package.json`.
   *Pattern rigor rule:* before trusting any "0 hits", validate the
   same pattern against a known-positive (a symbol you know is
   referenced). Check spelling variants: bare, quoted, backticked,
   kebab vs snake (`task-localize` / `task_localize`), and CLI vs
   internal names.
2. **Cross-process consumers registry** —
   `cross-process-consumers.md`, searched with backtick-aware
   patterns. Per Cardinal Rule 3 this is necessary but NOT sufficient.
3. **Contract tables** — `graph-schema.md`'s redb contract table
   (engine surfaces), `docs/reference/cli.md` (documented CLI),
   `docs/events-contract.md` (broker events), `docs/json-contracts.md`.
   (`cli-surface-v1.md` was retired 2026-08-01 with the delegated
   surface it froze — do not cite it.) A surface in a contract table is live even with
   zero present-day callers — external agents are licensed to call it.
4. **Test-gate investment** — exercised by `redb_cli.rs` gates or
   scenario suites (the golden corpus retired 2026-08-01). Recent gate investment is a
   strong signal the surface is a *destination*, not a leftover.
5. **Dying-by-design** — parity oracles and migration scaffolding
   (e.g. map-based navigation views used to verify redb views). Not
   dead until their migration's cleanup trigger fires; deleting them
   early destroys the verification the migration depends on.

Only candidates that clear all five are removable — with a
`Contract decision: <label>` line in the commit (the
`cross-process-contract` gate enforces this on every broker
submission once main syncs).

The `engineering_review` vs `literal_external_only` split in
`dead-code-baseline.md` is the same idea from the eval side: "no
external callers" is a benchmark answer, not a deletion license.

## Known inventory (owned, with deadlines)

| Item | State | Trigger / deadline |
|---|---|---|
| ~~`workspace_inspect` / `workspace_blast_radius` Python wrappers~~ | **RESOLVED 2026-08-07 sweep** — the wrappers died with Python; the only surviving mention is this table. The *engine* subcommands survive as predicted, spelled kebab (`workspace-inspect`, `workspace-blast-radius`) in `aethyme-engine-cli.rs`, and are the registry contract. Verified with both spellings per the pattern-rigor rule. | Closed. |
| 8 `#[allow(dead_code)]` sites (`graph_store.rs` handles/accessors, `linker.rs` fields) | Author-acknowledged, kept for symmetry | Review at redb-migration completion — if still uncalled when the parity scaffolding drops, remove the allows and the items in the same change. |
| Map-based navigation views (`node_view`, `callers_view`, …) | LIVE as parity oracles + residual map-backed paths | Owned by the redb phase-set: when its final gates stop comparing against `RepositoryMap`, the views AND their json renderers go in that cleanup. Do not pre-empt. |
| Test-only autofixer methods (e.g. `find_hardcoded_strings`, `save_patch_file`) | **PREDICTION MISSED — still open.** The 2026-07-28 entry assumed these would dissolve at Phase 5 because they would not be ported. They *were* ported: `find_hardcoded_strings` (`fix/fixers/i18n_scaffolder.rs:194`) and `save_patch_file` (`fix/patch.rs:327`) are `pub` in product code with their only callers inside `#[cfg(test)]` blocks — the parity port carried the methods and their tests across together. This is precisely the "tested but unused (green suites keep it alive)" class this plan says sweeps must catch. | **Operator decision, 2026-08-07 sweep**: removing them also removes tests that are part of the Phase 5 byte-parity evidence, days after that port landed — so the sweep reported rather than deleted. Decide: drop method+test, or keep and record why the `pub` surface is wanted. Re-audit the full set (the original count was 7) when deciding. |
| ~~Python transport stack (`engine.py` adapter, PyO3, `--engine-transport`, root discovery)~~ | **RESOLVED** — PyO3 and the transport went in Phase 1, the rest with `src/` in Phase 6. `engine_transport` has zero references repo-wide. Root discovery survives deliberately, reduced to what `enhance` needs for `{{AETHYME_ROOT}}` substitution. | Closed. |
| Unlinked-but-valid docs (`scorecard-guide.md`, `autofixers-guide.md`, onboarding, python-sdk tombstone) | Content valid, zero inbound links | Low priority: link from `docs/README.md` index or leave; not dead code. |
| Historical plan/report docs under `docs/architecture/`, `docs/reports/` | Dated records | Intentional archives — permanently out of scope. |

## Detection sweeps (trigger-driven, not calendar-driven)

Run a sweep when one of these fires — each migration wave is what
*creates* dead code, so hunt at the wave boundaries:

- ~~**After each python-retirement phase lands** (1–6)~~ — the
  retirement completed 2026-08-07 (Phases 0–7); its closing sweep is
  recorded below. Kept as the template for the next migration: the phase's
  deletion list is planned, but each phase also strands helpers the
  plan didn't enumerate. Sweep scope: the modules the phase touched.
- **When the redb phase-set drops its parity scaffolding**: the big
  one — map views, `RepositoryMap`-only paths, their renderers and
  tests. Owned by that effort; this plan just tracks the trigger.
- **After any `refactor!` commit** on the scale of the eval-stack or
  cloud removals: run the full audit recipe below within a week,
  while intent is fresh.

Sweep recipe (the 2026-07-17 audit, distilled):

1. `cargo check --all-targets` warning count must stay 0; any
   `#[allow(dead_code)]` added since last sweep needs a written reason.
2. `uvx vulture src/ scripts/ --min-confidence 80` — since retirement
   Phase 7 this applies to `packages/aethyme-eval` ONLY; `packages/
   aethyme` has zero Python, so a hit there means something has gone
   wrong. Every hit manually verified via the liveness protocol
   (vulture false-positives on click/pydantic/Protocol are known).
3. Rust pub-symbol sweep: for each crate, `pub fn`/`pub struct`
   defined-but-unreferenced across the workspace + Python side —
   compiler can't flag pub items.
4. Orphan artifacts: scripts/configs/docs referenced by nothing
   (respecting liveness authorities #2–#4), plus *referenced-but-
   broken* items — the more dangerous class; check that every
   registry/skill/Makefile reference still resolves.
5. Registry GC both directions: rows whose consumers died, and
   consumers that exist but aren't rowed (the load-context hook was
   the 2026-07-27 example).
6. Ledger the results in the commit messages and update this doc's
   inventory table.

## Guardrails already standing (what makes hunts cheap now)

- `cross-process-contract` broker gate: removals touching tracked
  symbols demand an explicit contract decision. Native since
  retirement Phase 6 (`aethyme broker check-contract`), and the gate
  builds it from the worktree under test.
- ~~Surface-freeze test~~ and ~~golden harness~~ **retired 2026-08-01
  (retirement Phase 6)**: both compared against a delegated Python
  surface that no longer exists. What replaced them: the
  implementation-blind suites in `aethyme-cli/tests/` and
  `aethyme-testkit/tests/` drive the built binary and assert on
  stdout/artifacts, and `cargo-test`'s trigger list was widened to
  `skills/**`, `docs/**`, `rust/grammars/**`, `scripts/eval/*.sh` and
  the PR template so a non-Rust edit still runs the suites that read
  those files.
- `verify-playground.sh`: deployed-artifact freshness, including
  "no Python invocation" checks on the wrappers and hook.

What these CANNOT catch — and the sweeps must: code that is tested but
unused (green suites keep it alive), contract surfaces whose contracts
should be *renegotiated* (a table row is a decision, not a law), and
docs/config drift outside tracked symbol names.

## Sweep log

### 2026-08-07 — post-retirement sweep (trigger: refactor at eval-stack scale)

Ran the full recipe after python-retirement Phases 0–7 (~10k lines).
Result: **the tree is clean; one removal, one repair, one item
re-opened.**

1. `cargo check --all-targets`: 0 warnings. The `#[allow(dead_code)]`
   count had drifted 8 → 9; the newcomer was `_path_marker`, a no-op
   stub in `aethyme-producers/tests/harness.rs` whose comment described
   a different thing entirely. Zero references — removed, back to 8.
2. vulture on `packages/aethyme-eval`: **no hits at the ≥80 threshold.**
   One 60% hit, `stats.py:METRICS` — defined once, referenced nowhere in
   repo. Left in place: below threshold, a public module constant in a
   package the operator leads, and an out-of-repo notebook could import
   it. Authority #1 cannot see out-of-repo consumers.
3. Rust pub-symbol sweep: 1,106 distinct `pub fn|struct|enum|trait|const`
   across all crates, **zero unreferenced.** Method: count `\b<sym>\b`
   workspace-wide excluding the definition line AND `use`/`pub use`
   re-export lines (a bare reference count hides items that are
   re-exported but never called). Per the pattern-rigor rule the filter
   was validated both ways before being trusted — a known-live symbol
   returned 7, a fabricated name returned 0.
4. Orphan / referenced-but-broken artifacts: no broken repo-relative
   path in `gates.toml`, any workflow, or the deployed wrappers. Three
   apparent hits were false positives on inspection — a historical
   comment, a build output, and a release-artifact glob. Doc links are
   covered continuously by `docs_hygiene.rs` since Phase 7.
5. Registry GC. *Consumers without rows:* none — every non-doc file that
   invokes an `aethyme` subcommand is rowed. *Rows citing dead things:*
   one real hit, repaired — the `explore-summary`/`verify-targets` row
   justified its scope by pointing at `cli-surface-v1.md`, retired in
   Phase 6, which would send a reader to a file that no longer exists.
6. Doc refresh: 3 inventory items closed, 1 re-opened (below), and the
   protocol's own stale citations fixed — it was still listing the
   surface-freeze test and golden harness as standing guardrails and as
   liveness authorities.

**Re-opened:** the test-only autofixer methods. The 2026-07-28 entry
predicted they would dissolve at Phase 5 by not being ported. They were
ported, and remain `pub` with test-only callers. Reported rather than
deleted — see the inventory row.

**Judgment recorded:** authority #4 (test-gate investment) does not
immunize a method whose only caller is its own test. This document
already says so — "code that is tested but unused (green suites keep it
alive)" is named as what sweeps must catch. Cite that line, not
authority #4, when this comes up again.

## Cardinal-rule boundary

Sweeps never touch eval-facing surfaces to make anything "look
cleaner" (Cardinal Rule 2), and Aethyme's own `analyze dead-code` /
`usage_boundary_query` may be used on *playground* targets as
candidate collectors — on Aethyme itself only as an unscored
diagnostic, never as the deletion authority.
