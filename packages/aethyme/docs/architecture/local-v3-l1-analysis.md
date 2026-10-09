# Local v3 — L1 analysis envelope and baseline adapter (#655)

Last Updated: 2026-10-09

This record covers the AQ0 analysis contract: how an analysis answer states its exact
subject and profile, and how much of it is known (plan §5.7, §6.11, §6.20). It is input
to **D38** (what makes an answer valid for an exact source), **D44** (the query and
degradation contract) and **D47** (schema extension without breaking old clients). It
consumes L0's D37 inventory (`local-v3-l0/d-analysis-inventory-d37.md`, PR #704).

Code: `aethyme-contracts::experimental_v0::analysis` (envelope) and
`aethyme-broker::impact_analysis_envelope` (baseline adapter). Golden vectors:
`aethyme-contracts/fixtures/experimental-v0/analysis.json`, from an independent Python
implementation.

## Envelope

An `aethyme.analysis-result/experimental-v0` record (#653 record layer):

| Member | Rule |
|---|---|
| `operation` | `resolve_symbol`, `find_references`, `describe_change` or `explain_impact` (§6.11). The last two need a `base_subject`; the others must not have one. |
| `subject`, `base_subject` | An exact `source_snapshot` (#652), a `legacy_git_revision` (full object id only; never a branch name), or a `changed_paths` set. |
| `profile` | `pinned` (the ID of an `aethyme.analysis-profile` record: producer, versions, languages, edge kinds, configuration digest) or `legacy` (a named engine behaviour). |
| `authority` | Always `advisory_analysis`. Anything else is refused (`not_advisory`). |
| `outcome` | available, unavailable, incompatible |
| `freshness` | exact, stale |
| `coverage` | complete_within_profile, partial |
| `limits` | within_limits, truncated |
| `gaps`, `reason`, `heuristic_confidence`, `provenance`, `limit_detail`, `result` | Supporting detail. A `result` requires `outcome: available`. |

The four dimensions are separate state fields: a result can be exact for its source and
partial in coverage at the same time (§5.7). As with every state field, an absent or
unrecognized value reads as **unknown**, never as the good value. An old producer that
never emitted `coverage` therefore yields "partial", and a producer with no dimensions at
all yields "unavailable".

A derived `Status` (complete, partial, truncated, stale, unavailable, incompatible) picks
the most severe reading, for consumers that need one word. **An empty result is evidence
of absence only when the status is complete** (`absence_is_evidence`): available, exact,
complete within its profile, and not truncated. This answers the acceptance criterion
"distinguish empty from missing, stale, partial, incompatible and truncated".

`compare_profiles` allows comparing or combining two results only under the identical
profile, and otherwise returns `incompatible_analysis_profile`. A `Cursor` pins one view
and one query, and an older cursor is refused (`stale_cursor`, T78).

A reader that passes a result on forwards the original record bytes. Re-encoding would
drop values it did not recognize but a newer reader would understand.

## Baseline adapter: `GraphImpactReport` → envelope

AQ0 asks that existing analysis be exposed "without stronger claims than its actual
coverage". The adapter's choices:

| Report | Envelope | Why |
|---|---|---|
| Revision (committed, Git object id) | `base_subject: legacy_git_revision` | The engine never computed raw-byte source identity (L0 slice D, row 5). A short revision cannot be bound and is refused. |
| List of changed paths (`diff_digest`) | `subject: changed_paths` | The query takes paths, not a candidate tree, so no candidate snapshot is claimed. |
| Engine version + mode | `profile: legacy aethyme-engine/<v>/graph-impact-<mode>` | Producer configuration is not pinned (L0 row 5), so only identical engine version and mode compare. |
| `complete` | exact, complete within profile, within limits | The only case where empty means "none". |
| `partial` | exact, **partial** coverage | The report does not separate "coverage gap" from "traversal cut short", so coverage is never upgraded. |
| `limits.truncated` | `limits: truncated` | Truncation is read from limits only. |
| `coverage.truncated` without `limits.truncated` | gap `coverage_gap` | The legacy flag is also set for coverage gaps. Reading it as truncation would mislabel the cause. |
| `stale` / `unavailable` | stale / unavailable; coverage and limits unknown; no result | The report already withholds impact paths. |
| `confidence` high/medium/low | `heuristic_confidence` only | Never feeds coverage or freshness. |
| Repository root path, explanations | Left out | Locators that may expose local paths (§5.3, T84). |

## Not decided here

- **D38 closes** only when real consumers (L3, Explore, the composer) show that the same
  source under different configurations cannot share a result. That needs a pinned
  profile from an actual producer. Today's engine has only legacy profiles.
- **D39** (symbol references across clones, overloads, package versions) needs
  `resolve_symbol`/`find_references` producers and two-clone fixtures (T66). The envelope
  carries their results; it does not define `SymbolRef` mapping records yet.
- **D44 closes** with Local, Explore, compositor and service fixtures reading one
  envelope. Only the broker's impact report is adapted so far.
- **D47:** the engine's "forever 1" fragment and overlay formats are wrapped, not
  bumped. `producer_version` is still never checked on read (L0 follow-up 3); a pinned
  profile is where that check will live.

## Tests

| Plan test | State |
|---|---|
| AQ0 (no stronger claim than coverage; partial/missing producer; old/new schema) | Envelope vectors plus adapter tests: complete-empty vs every degradation, an old producer missing dimensions, an unrecognized value, high confidence on a partial report, a coverage gap not read as truncation. |
| T78 (stopped producer, truncation, older cursor) | Unavailable, truncated and stale-cursor cases done. Timeout and an omitted language map to unavailable and partial, but have no producer fixture yet. |
| T85 (unknown fields and old/new consumers) | Through the record layer (#653) and the unknown/unrecognized dimension cases. |
| T65 / T66 | T65 baseline in #700. T66 needs symbol producers (D39). |
