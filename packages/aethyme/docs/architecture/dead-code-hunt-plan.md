# Dead-Code Hunt Plan — standing protocol

Last Updated: 2026-07-28

Status: ACTIVE. Companion to `python-retirement-plan.md` and
`../guides/dead-code-baseline.md`. Born from the 2026-07-17 audit
(~3.9k lines removed) and two near-misses that shaped the protocol
below.

## Why a standing plan instead of another sweep

The audit's root cause was structural: dead code accumulated because
nothing watched the seams. The seams are now guarded (contract gate,
surface-freeze test, golden harness), so the remaining work is (a) a
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
   `cli-surface-v1.md` (frozen surface), `docs/events-contract.md`
   (broker events). A surface in a contract table is live even with
   zero present-day callers — external agents are licensed to call it.
4. **Test-gate investment** — exercised by `redb_cli.rs` gates, the
   golden corpus, or scenario suites. Recent gate investment is a
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
| `workspace_inspect` / `workspace_blast_radius` Python wrappers | Zero Python callers; registry lists engine surfaces as live contract | **Operator decision needed**; forced at retirement Phase 1 when `engine.py` is dismantled. The *engine* subcommands stay either way (registry contract). |
| 8 `#[allow(dead_code)]` sites (`graph_store.rs` handles/accessors, `linker.rs` fields) | Author-acknowledged, kept for symmetry | Review at redb-migration completion — if still uncalled when the parity scaffolding drops, remove the allows and the items in the same change. |
| Map-based navigation views (`node_view`, `callers_view`, …) | LIVE as parity oracles + residual map-backed paths | Owned by the redb phase-set: when its final gates stop comparing against `RepositoryMap`, the views AND their json renderers go in that cleanup. Do not pre-empt. |
| Test-only autofixer methods (7, e.g. `find_hardcoded_strings`, `save_patch_file`) | Product-dead, test-kept | Dissolve at retirement Phase 5: not ported; their tests retire with the Python side. |
| Python transport stack (`engine.py` adapter, PyO3, `--engine-transport`, root discovery) | Live, scheduled | Retirement Phases 1 and 6 — already planned; do not double-track here. |
| Unlinked-but-valid docs (`scorecard-guide.md`, `autofixers-guide.md`, onboarding, python-sdk tombstone) | Content valid, zero inbound links | Low priority: link from `docs/README.md` index or leave; not dead code. |
| Historical plan/report docs under `docs/architecture/`, `docs/reports/` | Dated records | Intentional archives — permanently out of scope. |

## Detection sweeps (trigger-driven, not calendar-driven)

Run a sweep when one of these fires — each migration wave is what
*creates* dead code, so hunt at the wave boundaries:

- **After each python-retirement phase lands** (1–6): the phase's
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
2. `uvx vulture src/ --min-confidence 80` on each Python package —
   every hit manually verified via the liveness protocol (vulture
   false-positives on click/pydantic/Protocol are known).
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
  symbols demand an explicit contract decision.
- Surface-freeze test: the delegated CLI cannot lose commands
  silently.
- Golden harness: cross-process output drift (the 12-day `query deps`
  class) surfaces on the next capture/compare.
- `query deps`/`impact` and the router-subprocess test seam: the
  previously-untested surfaces now fail loudly.

What these CANNOT catch — and the sweeps must: code that is tested but
unused (green suites keep it alive), contract surfaces whose contracts
should be *renegotiated* (a table row is a decision, not a law), and
docs/config drift outside tracked symbol names.

## Cardinal-rule boundary

Sweeps never touch eval-facing surfaces to make anything "look
cleaner" (Cardinal Rule 2), and Aethyme's own `analyze dead-code` /
`usage_boundary_query` may be used on *playground* targets as
candidate collectors — on Aethyme itself only as an unscored
diagnostic, never as the deletion authority.
