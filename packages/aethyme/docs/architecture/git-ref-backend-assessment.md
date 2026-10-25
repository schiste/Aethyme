# Ref-heavy Git backend assessment

Last Updated: 2026-10-25

For #904, keep the files backend for existing Aethyme repositories. Reftable
substantially reduced enumeration versus loose files refs, while packed files
refs largely closed that gap. Lookup and worktree results were mixed. Production broker latency and the supported-client
matrix remain unqualified. This assessment changes no repository setting.

## Workload and results

The opt-in stdlib runner is
`packages/aethyme-eval/benchmarks/performance/git_ref_backends.py`. It reuses
the existing isolated command runner and records exact argv, return codes,
stdout/stderr, wall time, child CPU, and stdin length/digest for seeded refs.
It creates only its own temporary synthetic Playground under an existing
caller-selected scratch parent. Successful fixtures are removed; failures
retain their exact path and receipts. Each command has a twenty-second timeout.

Two sequential macOS 27.2 runs used an internal APFS/Apple Fabric volume and
an external APFS/USB volume. Each case had 2,000 synthetic agent branch refs
plus main, all pointing to one small commit. Git 2.55.0 and Apple Git 2.54.0
each created loose-files, packed-files and reftable variants. Seven consecutive
samples measured each operation. Packed-files variants ran Git's pack-refs before
measurement; the original loose-only trials remain retained as v1 evidence.
Files-backend fetch targets were populated before unchanged-fetch timing.

Values are median / maximum milliseconds, not p99 or cold-cache measurements.
Lookup includes Git process startup. Checkout materializes one tracked file:
this isolates ref inventory effects and does not model a large code checkout.

| Volume / client / backend | Lookup | Enumeration | Unchanged local fetch | Worktree add | Worktree remove |
|---|---:|---:|---:|---:|---:|
| Internal / 2.55 / files-loose | 11.08 / 15.90 | 87.72 / 101.48 | 293.17 / 316.70 | 24.97 / 33.36 | 26.21 / 30.14 |
| Internal / 2.55 / files-packed | 8.66 / 10.01 | 10.57 / 12.66 | 179.25 / 203.18 | 22.28 / 27.06 | 19.59 / 24.85 |
| Internal / 2.55 / reftable | 9.68 / 11.67 | 11.03 / 13.71 | 201.24 / 216.58 | 27.07 / 28.72 | 24.21 / 26.81 |
| Internal / Apple 2.54 / files-loose | 21.59 / 36.08 | 108.63 / 747.64 | 358.66 / 997.00 | 50.03 / 74.33 | 45.62 / 414.02 |
| Internal / Apple 2.54 / files-packed | 23.44 / 34.45 | 23.50 / 88.51 | 394.64 / 524.96 | 43.09 / 52.22 | 39.85 / 104.66 |
| Internal / Apple 2.54 / reftable | 19.23 / 35.75 | 23.32 / 38.54 | 241.85 / 314.48 | 33.54 / 61.29 | 35.60 / 63.16 |
| External / 2.55 / files-loose | 11.00 / 11.92 | 82.36 / 85.36 | 286.80 / 309.82 | 28.72 / 33.33 | 25.40 / 34.47 |
| External / 2.55 / files-packed | 11.91 / 13.17 | 14.91 / 19.55 | 256.27 / 268.61 | 34.59 / 46.89 | 26.81 / 32.15 |
| External / 2.55 / reftable | 11.52 / 13.14 | 13.61 / 15.71 | 268.35 / 394.69 | 34.26 / 46.12 | 29.92 / 34.34 |
| External / Apple 2.54 / files-loose | 19.17 / 23.19 | 96.56 / 104.04 | 324.66 / 357.90 | 38.36 / 78.38 | 34.08 / 37.37 |
| External / Apple 2.54 / files-packed | 20.64 / 23.55 | 24.81 / 41.89 | 265.86 / 396.18 | 39.52 / 60.00 | 37.73 / 80.38 |
| External / Apple 2.54 / reftable | 18.61 / 27.95 | 17.82 / 38.10 | 226.57 / 514.01 | 41.01 / 50.44 | 27.19 / 53.24 |

All twelve cases verified every branch name and object ID, alternate-client
reads in primary and linked checkouts, successful worktree removal, bundle
verification and restoration into a new files-backend repository, and a
conversion to the other backend and back with identical branch refs and a
successful full object check. Both revised runs completed 430 commands, removed
their fixtures and emitted no harness stderr. A prior 100-ref, one-sample
control passed all four original client/backend cases. The revised full runs
add the packed-files comparator without changing the original controls.

Raw reports retain initial-fetch observations separately. All tips share one
object history, so these measurements say nothing about transferring diverse
commit histories. Sequential case order, a single host, warm caches, and small
sample counts limit the comparison. Child CPU excludes persistent services.
No broker speedup, disk saving, hosted GitHub or IDE compatibility is claimed.

The measured harness SHA-256 is
`8a2e85dd6de59ccf55ab33b030a3d44899b0059876fcea4aacf3e96dff8c0e36`;
its shared command runner is
`59976cd67ab3200b072f12a4fb447ff5709a0c89a940e7d7be23266fb28f6739`.
Retained evidence IDs are `nonv3-ref-backends-internal-20261025-v2` and
`nonv3-ref-backends-external-20261025-v2`, including complete command receipts.

## Compatibility and recovery boundary

Aethyme's declared minimum remains Git 2.38 in
`aethyme-broker/src/release_compatibility.rs`. Git integrated reftable as an
initialization backend in its [2.45 release notes](https://raw.githubusercontent.com/git/git/v2.45.0/Documentation/RelNotes/2.45.0.txt).
The two installed clients do not establish behavior at Aethyme's lower floor.
There is no minimum-client binary, IDE matrix or hosted GitHub exercise in
these trials. Raising a version number alone would not qualify those consumers.

The targeted first-party boundary audit found `local_branch_tips` using
`for-each-ref`, checked updates using `update-ref` with an expected old ID,
and operation markers resolved through `rev-parse --git-path`. No production
broker Rust matches for direct `packed-refs` or `join("refs")` reads were found
in this bounded search. That is not proof about every dependency or script.
Two existing ignored `tests/reftable_git.rs` regressions cover branch listing
and checked updates; source inspection is not execution of those tests. They
still need explicit native execution during combined qualification.

Bundle restoration here preserves objects and branch refs, not live broker
state, worktree registrations, reflogs or interrupted migration journals.
Conversion was performed only after all linked checkouts were removed and with
no concurrent writers. It is not a safe production migration procedure.
If reconsidered, first qualify actual broker status and lifecycle operations,
all supported clients and interrupted conversion recovery with consistent
database/Git backups. Never restore an old backup over newer agent activity.
Do not enable the unrelated `feature.experimental` bundle as a substitute.
