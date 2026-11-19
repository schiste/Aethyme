# Paired Codex trial with trusted Aethyme hooks

Aethyme v0.8.137 took **24.7% longer at the median** and **49.4% longer
at p90** than direct Git. Both arms completed 12 of 12 assignments and
needed one textual-conflict repair. This trial found zero avoided repair
turns and zero reported duplicate-work interventions.

## Setup

- Latest published release at launch, Aethyme v0.8.137, built from
  `a4856b018d18bbda1d05e93871d50337828e3afe`. Binary SHA-256
  `74e28f746bdc858164257fcec8a88fef1cb00fdaf2c41d52cc47b883799f899e`.
- Codex CLI 0.170.0, `gpt-6-sol`, low reasoning, workspace-write sandbox.
  The broker arm used the installed Aethyme plugin with its existing hook
  trust records in an isolated Codex home. The direct Git arm disabled the
  plugin. The broker recorded 39 activity signals across 13 initial and
  repair turns, with three signals per turn.
- Six independent Playground groups, each with two concurrently launched
  Codex workers. Three groups had deliberate overlaps. The other three had
  disjoint edits. Every arm got a fresh repository, local origin and worktrees.
- Both arms enforced owned paths, exact-head publication, the same visible
  tests, hidden checks, integration order and one-turn repair budget.
  Codex protected the linked Git metadata in a preflight check. The
  coordinator staged explicit paths and published for both arms while
  workers authored and tested in their original sandboxes.

The initial seed was 19. Four groups used their original scheduled runs.
The original textual group's repair harness compared the checkout to the
wrong commit. A diagnostic retry then overlapped with the original
report/text group and one worker timed out. Both groups were rerun without
overlapping model workloads, using baseline-first order. Across the
reported runs, four groups ran baseline first and two ran broker first.
In the final report/text run, detached broker maintenance raced fixture
deletion after task results and telemetry were saved. Both tasks retained
passing results. The harness now retries that teardown outside task timing.

## Matched task results

Wall time starts at checkout setup and ends after integration and any
session closure. It includes time waiting for the preceding candidate.
Tokens come from Codex's native usage records, including repair turns.
Medians and nearest-rank p90 use the same 12 completed assignments in each arm.

| Measure | Direct Git median | Broker median | Change | Direct Git p90 | Broker p90 | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Completion time | 41.33 s | 51.54 s | +24.7% | 46.74 s | 69.84 s | +49.4% |
| Uncached input tokens | 7,498.5 | 9,100.5 | +21.4% | 15,122 | 18,850 | +24.7% |
| Cached input tokens | 106,176 | 114,560 | +7.9% | 138,240 | 292,352 | +111.5% |
| Output tokens | 1,239 | 1,315.5 | +6.2% | 1,459 | 1,892 | +29.7% |

| Group | Direct Git median | Broker median | Repair turns, Git / broker |
| --- | ---: | ---: | ---: |
| Semantic overlap | 40.38 s | 43.43 s | 0 / 0 |
| Disjoint postal / shipping | 43.22 s | 40.37 s | 0 / 0 |
| Shared registry file | 34.95 s | 64.52 s | 0 / 0 |
| Disjoint search / loyalty | 39.89 s | 46.99 s | 0 / 0 |
| Disjoint report / text | 38.54 s | 61.05 s | 0 / 0 |
| Textual overlap | 58.74 s | 84.86 s | 1 / 1 |

All 24 initial candidate branches passed their assignment checks. The
integrator detected one textual conflict per arm before merge. Both were
repaired and the final trees passed. No human intervened in a reported
task. The integrator recorded those conflicts independently of the broker.

## Broker command attribution

| Foreground command | Calls | Median | p90 |
| --- | ---: | ---: | ---: |
| `start` | 12 | 1.279 s | 2.580 s |
| `advance` | 13 | 1.136 s | 3.577 s |
| `status` | 7 | 0.408 s | 1.510 s |
| `note send` | 4 | 0.444 s | 0.711 s |

The release attributed 43 commands to the 12 closed broker tasks. Their
55.40 seconds of command time comprised 31.77 seconds in child commands,
0.43 seconds in queues and 23.20 seconds of broker and unattributed time.
They emitted 133,158 stdout bytes and recorded zero broker retries.
The release omitted hook timings and the finish rows for removed checkouts.
The attribution patch adds those measurements. Detached GC recorded nine
commands with 80.84 seconds of concurrent work in aggregate. That work
can overlap other measurements, so adding it to task wall time would
double-count some time.

## Shared verification

The live fixture has no declared broker gates, so it had zero opportunities
to reuse an identical-tree broker verdict. The separate
[`paired_verification.py`](../paired_verification.py) probe pairs two
simultaneous gate requests against two direct suite invocations on the same
tree. The fixture was staged and its trust check confirmed that an operator
must approve its single unit-test command before the probe can run. Shared
verdict reuse, child executions avoided and the resulting elapsed time
remain unmeasured in this release trial.

The six groups exercise the entire configured workflow on small local
tasks. They give a descriptive comparison of this mix of work. The
November 18 trial used a different task mix and publication path, and
this trial did not isolate the effect of hooks from the rest of the broker
workflow.
