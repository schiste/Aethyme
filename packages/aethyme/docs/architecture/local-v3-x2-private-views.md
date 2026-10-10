# Local v3 — X2 private analysis views: prototype and findings (#684)

Last Updated: 2026-10-10

X2 is an **optional** package (plan §15.7). This record covers the prototype: one immutable
analysed base plus one private overlay for a retained candidate snapshot (§6.14–6.16). It
is input to **D42** (reusing a base without changing canonical state), **D43** (what
invalidates facts) and **D49** (what benefit justifies incremental analysis). It builds on
L0 slice D (`local-v3-l0/d-analysis-inventory-d37.md`, PR #704), AQ0 (#710) and the L2
state root, archive and capture (#714–#716).

Code: `aethyme-broker::analysis_view`, plus a reuse hook in
`aethyme-graph-indexer::index_repo_to_disk_cached`. A library API only; nothing calls it.

## What was built

### Where views live

`<collaboration project dir>/views/<view>/`. That is outside every cleanup root
(`local-v3-l2-state.md`), outside the repository, and outside the canonical
`.aethyme/graph` fragments and producer `_overlays` (L0 slice D rows 3 and 6). The host
cache was rejected, because the operating system can purge it under a pinned reader. The
GC (#659) does not own `views/`: it reports it as unknown and never removes it. Views have
their own lifecycle (below).

### Facts, masks and replacements

L0 slice D established that every language indexer reads only the repository name, and the
walker records only path, language and byte size. A file's extraction is therefore a
function of (repository name, path, language, content, indexer profile). The pipeline gained
an `ExtractionCache` hook: it asks for exactly that unit and records every extraction it
computes. With no cache it behaves exactly as before.

| Term | Meaning here |
|---|---|
| Base | A sealed full analysis of snapshot S0 under profile P. It keeps every unit's extraction, keyed by a content key over (P, repository name, path, language, content). |
| Overlay | A view of candidate SC on one base. It reuses a base extraction wherever the key matches, extracts the rest, and seals its own linked facts. |
| Replacements | The units an overlay extracted itself: changed, added and moved paths. |
| Mask | The base units the overlay does not reuse: changed, deleted and moved-away paths. |
| Derived facts | Non-code relationships and linking. They are cross-file, so they are recomputed over the merged set for every view. |

Recomputing derived facts every time over-invalidates on purpose. §6.15 allows that and
never the reverse. It is what makes a renamed export (T72), a newly present name or a
deleted target (T73), and a move unable to leave a stale or ghost link. Precise
derived-fact invalidation is a later optimisation and needs its own fixtures.

### Profile

`ViewProfile` covers producer, producer version, repository name, and the parser registered
for each language. Its digest is part of every content key and of the pinned AQ0 profile
record that answers carry. A base under another profile is never reused (T90): the build
refuses with `incompatible_analysis_profile`, and `view_for_candidate` falls back to a fresh
analysis.

### Generations, readers and lifecycle

- **Building.** A generation is built in `building-<n>-<pid>/`: the retained snapshot is
  materialised there, indexed, linked, and the source dropped. A build holds the view's
  `.build.lock`. The snapshot's own `.aethyme/` is removed before indexing; the walker skips
  it anyway, and the indexer writes there.
- **Sealing.** The manifest records identity, base reference, units, mask, replacements,
  coverage, a digest over the linked fragments, and costs. It is written and synced, and an
  empty `.pin` is created.
- **Publication.** One rename turns `building-…` into `gen-<n>/`; only then does `CURRENT`
  name it (temporary file plus rename). A crash before either step leaves a directory no
  reader opens (T76).
- **Readers.** `open_view` reads `CURRENT`, takes a shared lock on that generation's `.pin`,
  and confirms the generation still exists. A newer generation never changes what a pinned
  reader sees.
- **Sweep.** It reclaims build debris and superseded generations that nobody pins. It never
  reclaims the current generation, and skips everything while a build runs.
- **Retire.** It is refused while an overlay is built on the view (`has_dependents`) or a
  reader pins it (`pinned`) (T86). It never touches retained source.
- **Fork transient.** A pin, or a just-finished build's lock, can outlive its holder by an
  instant when another thread of the same process forks a child: the child shares the lock
  until it execs. Right after a release, `pinned` from retire and `pinned` or `building` from
  sweep are safe to retry, and the tests do.

States map to §6.16 as follows:
- *building* is an unpublished directory.
- *ready* and *partial* are published, with partial meaning the indexer's coverage was not
  complete.
- *failed* is a build error that leaves only debris.
- *retired* is the `RETIRED` marker.
- *obsolete* is a superseded generation awaiting sweep.

### Depth

Exactly one base plus one overlay. An overlay on an overlay is refused
(`unsupported_stacking`) (T77). Flattening means building a new base, which is a fresh
analysis. `view_for_candidate` does that automatically, and says so in its outcome.

### Answers

`find_references` and `explain_impact` traverse calls, imports and references over the
linked facts, and answer through the AQ0 envelope:
- **Subjects:** the exact snapshot. For impact, the changed-path set against the snapshot.
- **Profile:** the pinned record.
- **Freshness:** exact.
- **Coverage:** complete within profile only for a ready view, and for references only when
  nothing named the symbol without resolving. Otherwise partial, with gaps such as
  `unresolved_references:<n>`, `view_partial` and the indexer's coverage gaps.
- **Limits:** truncation from the query budget.
- **Provenance:** view, kind, generation, base view and generation, mode (incremental or
  fresh), and the fragment-set digest.

### Isolation

Nothing here writes a repository, its canonical fragments or producer overlays, or calls the
graph refresh. The active-session refresh guard (L0 slice D row 6) is therefore never reached,
so nothing can bypass it. T75's test builds two candidates' overlays at once from one base,
checks that each answers for its own snapshot, and checks that the repository has no
`.aethyme/` and a clean status afterwards.

## Results

| Plan test | Result |
|---|---|
| AQ2 / T71 | For five mutations, the overlay's linked facts are byte-identical to a fresh analysis of the same snapshot (same fragment-set digest), and all 10 fixture queries answer identically once provenance is removed. The mutations are a body edit, an export rename, an absent name added, a deleted target and a moved file. FX19 does not exist yet, so the fixture is this package's own. |
| Independent oracle | Interactions annotated from the source (b calls a.f, c calls b.main, c's call to the absent h is not invented, the impact of a.py reaches b and c) hold on the base. |
| T72 | Export renamed while the consumer's bytes are unchanged: the consumer's extraction is reused, linking is recomputed, the call no longer resolves, and the answer equals fresh analysis. |
| T73 | Absent name added, target deleted (no ghost edge; the reference becomes an `unresolved_references` gap), file moved: each equals fresh analysis. |
| T74 | By construction for the structural profile: a lockfile or config file is its own unit, and every cross-file fact is recomputed. A producer configuration change is a profile change (T90). Not separately fixtured. |
| T75 | Two concurrent overlays from one base are isolated; the repository and canonical state are untouched. |
| T76 | A crash at each of four points (after indexing, after linking, before publication, before `CURRENT`) publishes nothing. Readers keep the previous generation and its answers. Sweep removes the debris. A pinned reader keeps its generation while a newer one publishes; that generation is reclaimed only after release. |
| T77 | An overlay on an overlay is refused; the candidate falls back to a fresh analysis. |
| T83 | Partial: readers never wait for a build. Analysis-slot reservation and priority against stop and revoke are not built. |
| T86 | Retire is refused under an overlay or a reader; retained source still reconstructs afterwards. |
| T90 | A base from another producer version is never reused. An unrelated edit re-extracts exactly one unit. |
| T92 | Not covered: FX24 and predeclared thresholds belong to D49's evaluation. Costs are below. |

Neuter checks: 12 deliberate breaks, each failing at least one test.
- the content key without content
- no profile check
- no stacking check
- no relinking of overlays
- `CURRENT` written before publication
- no reader pin
- sweep reclaiming the current generation
- retire without the dependents check
- retire without the pin check
- the unresolved gap dropped
- an empty mask
- placeholders counted as resolved references

## Costs (measured)

Fixture: 400 generated Python files (21 functions each, every file importing and calling the
next), so about 8,400 functions. The candidate rewrites one file. The measurement test is
`analysis_view::tests::measure` (ignored by default). Medians of three runs on a heavily
loaded host (load average 12–16, other builds running), so absolute times are noisy; the
ratios are what matter.

| View | Build | Materialise | Index | Link | Extracted / reused | Facts | Extractions | Impact query |
|---|---|---|---|---|---|---|---|---|
| Base (S0) | 5.6 s | 0.15 s | 3.0 s | 1.9 s | 400 / 0 | 5.7 MB | 4.5 MB | 0.26 s |
| Overlay (SC on S0) | 11.1 s | 0.49 s | 3.3 s | 2.2 s | 1 / 399 | 5.7 MB | 443 B | 0.15 s |
| Fresh (SC) | 6.8 s | 0.37 s | 3.4 s | 2.5 s | 400 / 0 | 5.7 MB | 0 | 0.25 s |

The overlay's facts equal the fresh analysis's in every run (same fragment-set digest).

**Reuse saved almost no indexing time.** Parsing 1 file instead of 400 left the index phase
at about the same cost (3.3 s against 3.4 s; across runs 2.9–3.6 s against 3.2–3.4 s).
For this parser the expensive parts are the ones reuse cannot skip:
- walking the tree;
- reading every file to compute its content key;
- decoding the reused extractions;
- non-code relationships;
- writing every fragment.

Linking (about 2 s) runs over the whole snapshot either way. The overlay's total build time
varied most with host load (5.2–13.5 s).

## Findings

1. **Agreement is exact, and cheap to check.** Because per-file extraction is file-local
   and derived facts are always recomputed, an overlay's facts are byte-identical to fresh
   analysis. The fragment-set digest is a complete AQ2 oracle for this profile, with no
   normalisation beyond provenance.
2. **What is not reused dominates.** Parsing is the only work an overlay saves. Walking,
   reading every file for its content key, decoding reused extractions, non-code
   relationships, fragment writes and linking still cover the whole snapshot. With the
   structural Python parser, those cost as much as the parsing saved (see *Costs*).
3. **Storage is per view, not per overlay.** Each generation stores its full linked fact
   set, because linking is recomputed. Only extractions are overlay-sized. A true
   overlay-only fact store needs per-fact support tracking (§6.15), which this prototype
   deliberately does not attempt.
4. **The structural profile has known blind spots, and they stay visible.** Rust method
   calls, PHP calls and the lossy `Tests`→`References` collapse (L0 slice D rows 8, 8b, 9)
   are outside the profile, not errors in the view. Unresolved names are reported as gaps,
   never as absence.
5. **Lock lifetime under fork.** A flock can outlive its release for an instant when a
   sibling thread forks, which happened in about 1 in 100 parallel test runs. Sweep then
   reports `building` or `pinned` and skips, which is the safe answer. Every reclamation
   path must treat both as retryable, never as an error.

## Recommendation

**Do not proceed with X2 as an incremental-performance feature for the structural
profile.** The correctness side holds:
- an overlay's facts are byte-identical to fresh analysis;
- isolation, crash safety, reader pinning and the lifecycle are tested.

D49's benefit side does not. On a 400-file fixture, reusing 399 of 400 extractions saved no
measurable indexing time, linking is unaffected, and every generation still stores the full
linked fact set.

What is worth keeping is the **isolated, exact-snapshot analysis**: the fresh view. It gives
X3 (#685) and L3 a sealed, pinned, AQ0-enveloped answer for any retained snapshot without
touching canonical state or the refresh guard.

Revisit incremental reuse only when one of these holds:
1. a producer whose per-unit extraction is expensive joins the profile, such as an X1
   semantic import (SCIP or rust-analyzer), where parse time would dominate;
2. linking becomes incremental, with per-fact support so derived facts can be masked
   instead of recomputed;
3. a measured workload shows parse time dominating, under thresholds fixed beforehand.

Until then, the extraction hook is cheap to keep and costs nothing when no cache is given.

## Not decided here

- **D42** needs the T83 slot reservation, real candidate workloads, and a decision on
  whether `views/` should become a GC retention class (#659's `AnalysisView` pin is the
  natural hook).
- **D43** needs FX19 and its independent oracles, and precise derived-fact invalidation if
  per-overlay fact storage is ever wanted.
- **D49** needs FX24 thresholds fixed before measuring, a representative corpus, and the X1
  semantic profile for comparison.
- **Promotion.** A view is never promoted into canonical graph state (§6.16); that remains
  the refresh lifecycle's job.
