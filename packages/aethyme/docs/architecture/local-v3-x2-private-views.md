# Local v3 — X2 private analysis views: measured, overlays dropped (#684)

Last Updated: 2026-10-10

X2 is an **optional** package (plan §15.7). It asked whether a private incremental view,
one immutable analysed base plus one overlay per candidate (§6.14–6.16), is worth having. It
was prototyped and measured. The overlay was exactly as correct as fresh analysis, but gave
no measurable gain. **The user decided to stop X2 as an incremental feature and keep fresh
exact-snapshot views.** This record covers the measurement, that decision, and what remains:
isolated fresh views and their reclamation class. It is input to **D42**, **D43** and
**D49**.

Code: `aethyme-broker::analysis_view` (fresh views) and the `view` class in
`aethyme-broker::collaboration_gc`. The indexer is unchanged; the extraction-reuse hook the
prototype added was reverted.

## Decision (user, 2026-10-10)

- **Stop** incremental overlays for the structural profile. `build_overlay` refuses with
  `overlays_unsupported`, and stacked views do not exist.
- **Keep** fresh exact-snapshot views as the way X3 (#685) and L3 ask about a candidate
  without touching canonical graph state.
- **Revisit** incremental reuse only for one of:
  - a producer whose per-file extraction is expensive, such as an X1 semantic import (SCIP,
    rust-analyzer);
  - incremental linking with per-fact support, so derived facts can be masked instead of
    recomputed;
  - a measured workload where parsing dominates, under thresholds fixed beforehand (D49).
- **Views become a GC retention class.** They are derived from retained source and can be
  rebuilt.

## What was measured

The prototype keyed each file's extraction by (profile, repository name, path, language,
content). L0 slice D established that a language indexer reads nothing else. An overlay
reused the base's extraction wherever the key matched and recomputed every cross-file fact
(non-code relationships, linking) over the merged set.

**Correctness held exactly.** For five mutations, the overlay's linked facts were
byte-identical to a fresh analysis of the same snapshot, with the same fragment-set digest,
and all 10 fixture queries answered identically. The mutations were:
- a body edit;
- an exported name renamed while its consumer stayed unchanged (T72);
- a previously absent name added (T73);
- the target deleted (T73);
- the file moved.

**The gain was nil.** Fixture: 400 generated Python files (21 functions each, every file
importing and calling the next), with a one-file change. Medians of three runs on a heavily
loaded host (load average 12–16), so absolute times are noisy:

| View | Build | Materialise | Index | Link | Extracted / reused | Facts | Extractions | Impact query |
|---|---|---|---|---|---|---|---|---|
| Base (S0) | 5.6 s | 0.15 s | 3.0 s | 1.9 s | 400 / 0 | 5.7 MB | 4.5 MB | 0.26 s |
| Overlay (SC on S0) | 11.1 s | 0.49 s | 3.3 s | 2.2 s | 1 / 399 | 5.7 MB | 443 B | 0.15 s |
| Fresh (SC) | 6.8 s | 0.37 s | 3.4 s | 2.5 s | 400 / 0 | 5.7 MB | 0 | 0.25 s |

Parsing 1 file instead of 400 left the index phase at about the same cost: 3.3 s against
3.4 s, and 2.9–3.6 s against 3.2–3.4 s across runs. For this parser, the work reuse cannot
skip costs as much as the parsing it saves:
- walking the tree;
- reading every file for its key;
- decoding the reused extractions;
- non-code relationships;
- writing every fragment.

Linking took about 2 s and ran over the whole snapshot either way. Every generation stored
the full linked fact set, because linking is recomputed.

## What remains: fresh exact-snapshot views

### Where views live

`<collaboration project dir>/views/<view>/`, with one view per (snapshot, profile). That is:
- outside every cleanup root (`local-v3-l2-state.md`);
- outside the repository;
- outside the canonical `.aethyme/graph` fragments and producer `_overlays` (L0 slice D rows
  3 and 6).

The host cache was rejected, because the operating system can purge it under a pinned
reader.

### Building and publication

- A generation is built in `building-<n>-<pid>/` under the view's `.build.lock`. The
  retained snapshot is materialised there, its own `.aethyme/` is removed, and the unchanged
  structural indexer and linker run on it. Only the linked facts are kept.
- The manifest is sealed. It records the snapshot, profile, coverage, a digest over the
  linked fragments, and per-phase costs.
- An empty `.pin` is created, then one rename publishes `gen-<n>/`, and only then does
  `CURRENT` name it (temporary file plus rename). A crash before either step publishes
  nothing (T76).

### Readers and profile

- `open_view` pins the `CURRENT` generation with a shared lock on its `.pin`. It takes that
  pin under the archive's store-wide shared lock, like every other pin, so a reclamation
  apply (exclusive) never races a new reader.
- `open_view_for` refuses a view built under another profile (`incompatible_analysis_profile`,
  T90). Another profile gets its own view.
- `view_for_snapshot` opens the view for a snapshot, or builds it when there is none.

### Answers

`find_references` and `explain_impact` traverse calls, imports and references over the
linked facts, and answer through the AQ0 envelope:
- **Subjects:** the exact snapshot (for impact, the changed-path set against it).
- **Profile:** the pinned profile record.
- **Freshness:** exact.
- **Coverage:** complete within profile only for a ready view, and for references only when
  every use of the name resolved. Otherwise partial, with gaps such as
  `unresolved_references:<n>`. A deleted target is a gap, never complete-and-empty.
- **Limits:** truncation from the query budget.
- **Provenance:** view, generation, mode `fresh`, and the fragment-set digest.

### Holds and retirement

- A consumer such as an X3 report or an L3 context records a **hold** with `hold_view`,
  until a time or until `release_view_hold`. An unreadable hold file counts as live.
- `retire` is refused while the view is held (`held`) or its `CURRENT` generation is pinned
  (`pinned`).
- A retired view no longer opens. Building the same snapshot again revives it.

### Isolation

Nothing here writes a repository, canonical fragments or producer overlays, or calls the
graph refresh. The active-session refresh guard is therefore never reached, so nothing can
bypass it (T75).

## GC class `view`

`collaboration_gc`'s survey covers `views/` with the same digest-bound plan, apply and
resume journal as archive objects. View bytes are reported as their own retention class.

| Entry | Reclaimed when | Never while |
|---|---|---|
| `CURRENT` generation of a live view | never; counted as retained `view` bytes | — |
| A generation of a retired view | immediately | pinned; view held; view being built |
| A superseded generation | `CURRENT` has named a newer one for longer than grace | pinned; view held; view being built |
| `building-*` directory | older than grace and nobody holds `.build.lock` | the build lock is held |
| Symlinks, unexpected files, an unreadable `CURRENT` | never: reported as unknown, nothing in that view is judged | — |

- **Apply** holds the build lock of every view its plan touches before re-surveying, so no
  build can revive or publish into a view while its directories move. The survey treats
  those locks as apply's own, not as a running build.
- **Resume** restores and re-checks moved directories. A directory whose name a later build
  reused keeps the newer copy.
- **Fork transient.** A pin or a just-finished build lock can outlive its holder by an
  instant when another thread of the same process forks a child: the child shares the lock
  until it execs. Reclamation then protects the entry, which is the safe answer. The tests
  retry, and callers should treat `pinned` right after a release as retryable.

## Tests

| Plan test | Result |
|---|---|
| AQ2 / T71–T73 | Measured on the prototype: overlay and fresh analysis agreed byte for byte across five mutations. Moot now that overlays are dropped. |
| Independent oracle | Annotated interactions hold on a fresh view: b calls `a.f`, c calls `b.main`, c's call to the absent h is not invented, and the impact of `a.py` reaches b and c. |
| T73 (absence) | A deleted target is an `unresolved_references` gap with partial coverage; absence is never claimed. |
| T75 | Two candidates' views built concurrently are isolated. The repository has no `.aethyme/` and a clean status afterwards. |
| T76 | A crash at four points publishes nothing, and readers keep the previous generation and its answers. A pinned generation and `CURRENT` are never reclaimed. Debris goes only after grace. |
| T77 | Overlays and stacking are refused (`overlays_unsupported`). |
| T83 | Partial: readers never wait for a build. Analysis-slot reservation is not built. |
| T86 | Retire is refused under a hold or a reader. A retired view's generations are reclaimed; retained source still reconstructs. |
| T90 | A view answers only under its own profile; another producer version gets a separate view. |
| T92 | Not covered (FX24, D49 thresholds). |

The GC `view` class has tests for each rule:
- `CURRENT` and pinned generations kept;
- a superseded generation waits for grace;
- stale build debris reclaimed;
- held and building views keep everything, and an expired hold no longer protects;
- retired generations go immediately;
- a symlinked view is unknown and untouched;
- an interrupted apply is finished by resume.

Neuter checks: 14 deliberate breaks, each failing at least one test.
- *GC:* `CURRENT` reclaimable, pin ignored, hold ignored, build lock ignored, superseded
  without grace, debris without grace, retired not reclaimable, links followed (both guards),
  apply's own build locks not recognised, view bytes not counted.
- *Views:* hold expiry ignored, retire ignoring holds, overlays accepted, profile check
  removed.

Two are not neuter-testable here:
- **The shared lock around pin acquisition:** removing it only opens a race window that no
  deterministic test can hit.
- **Resume's newer-directory branch:** it needs a build to reuse a trashed generation's name
  mid-resume.

## Not decided here

- **D42** needs T83 analysis-slot reservation and priority against stop and revoke.
- **D43** and **D49** reopen only on the revisit conditions above. Any future incremental
  attempt needs FX19 and FX24, independent oracles and thresholds fixed in advance.
- **Promotion:** a view is never promoted into canonical graph state (§6.16); that remains
  the refresh lifecycle's job.
