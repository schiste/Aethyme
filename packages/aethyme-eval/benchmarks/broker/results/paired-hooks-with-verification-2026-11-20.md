# Paired Codex trials with exact-tree verification

## Duplicate graph hashing

The gate optimization merged in [PR #1878](https://github.com/schiste/Aethyme/pull/1878).
The foregone hash took 64 ms cold and 57 ms warm per request at the median.
All 24 optimized requests still hashed the exact gate inputs, and both
builds reused one execution per pair in all 12 pairs. Cold is the first
measured pair per fixture, and warm is the two following pairs.

| Two concurrent requests | Pairs per build | Broker before median / p90 | Broker after median / p90 | Direct control before → after |
| --- | ---: | ---: | ---: | ---: |
| Cold | 4 | 796 / 1,291 ms | 993 / 1,252 ms | 80 / 114 → 115 / 141 ms |
| Warm | 8 | 817 / 1,694 ms | 881 / 1,533 ms | 85 / 192 → 92 / 356 ms |

The concurrent control also slowed during the optimized runs. The concurrent
wall-time effect remains unresolved. In a separate single-session warm
benchmark, alternating the two builds on the same non-cached gate reduced
median dispatch from 593 ms to 520 ms (12.2%). The paired median saving was
61 ms, with 18 wins in 20 pairs. The new spans measured profile capture at
158 ms cold and 131 ms warm. Pre-spawn command setup took 4 ms cold and
2 ms warm. [PR measurements](https://github.com/schiste/Aethyme/pull/1878)

The hosted probe used two independent requests for the same exact tree, gate
definition, and execution profile. The broker executed six gates and reused
six verdicts across six pairs. The direct arm executed twelve gates.
Reused gate work was 472 ms in aggregate. Broker pair wall time was
326 / 1,301 ms median / p90 versus 53 / 63 ms for the direct arm. One
verification was enough in each pair, but the gates were too cheap to repay
the coordination overhead. [Hosted benchmark artifacts](https://github.com/schiste/Aethyme/actions/runs/44692666707)

## Live trial method

The [paired live harness](../paired_live.py) gives each worker an isolated
worktree, an explicit set of allowed paths, and the same acceptance checks.
It alternates arm order by seeded assignment group. The strongest independent
group runs six tasks concurrently. Separate pairs exercise textual overlap,
semantic overlap, and harmless shared-file edits. The broker arm installs the
stock trusted Codex hooks. The direct arm gets the same task and worktree
rules with the plugin disabled. The coordinator stages, commits, and
publishes only owned edits. It checks the integrated candidate before updating
the disposable origin.

The cheap gate runs the existing Playground unit suite once. The substantial
gate repeats it 40 times to expose the effects of longer verification.
Both arms run the same gate command for the same integration check. Broker
submit independently derives the candidate and its gate result must identify
the exact tree prepared by the direct Git integration. Task time includes
worktree creation, authoring, publication, repairs, integration checks, and
broker finish where applicable. Token counts are measured Codex API usage.
P90 uses nearest rank. The timing and token tables include assignments that
completed in both arms, while completion and repair counts include all trials.

With `--verification cheap` or `--verification substantial`, the fixture
scratch directory defaults to the OS temporary directory for isolated trust
setup. `--wide` enables the six-way group. `--preverify` optionally asks
the tested broker to start early verification after publication.

## Published release with trusted hooks

The 12 paired assignments ran on Aethyme v0.8.140
(`52593980411331d4ed9e30d62ae2c30ac0e958fe`), Codex CLI 0.170.0, and
`gpt-6-sol` with low reasoning effort and assignment seed 19. The two
arms each passed all 12 exact-tree checks and completed every assignment.
The broker arm recorded 39 activity signals across the 12 sessions. Both
arms detected and repaired the same single textual conflict. The semantic
and shared-file groups passed their combined acceptance checks. Neither
arm needed human intervention. All 12 integration candidates in each arm
required their own verdict. The broker recorded zero gate cache hits.

Each cell below shows median / p90 per matched completed assignment. The
last column compares the broker to direct Git in the corresponding quantile.

| Measure | Direct Git | Aethyme | Change |
| --- | ---: | ---: | ---: |
| Task duration | 51.36 / 66.19 s | 88.23 / 116.97 s | +71.8% / +76.7% |
| Uncached input tokens | 8,004.5 / 9,824 | 8,511 / 24,510 | +6.3% / +149.5% |
| Cached input tokens | 102,848 / 113,792 | 123,200 / 231,424 | +19.8% / +103.4% |
| Output tokens | 1,206.5 / 1,533 | 1,209 / 1,465 | +0.2% / −4.4% |
| Exact-tree verification wall time | 4.77 / 6.85 s | 11.49 / 20.12 s | +140.9% / +193.8% |

The median of the 12 paired task-duration differences was +39.72 s.
None of the 12 broker tasks finished faster than its direct counterpart.
The large tail in input usage was concentrated in the overlap groups.
In the six-way independent group, the broker used 7,960.5 median uncached
input tokens and 1,034.5 output tokens, versus 8,009 and 1,259.5 direct.
Final batch acceptance checks ran after the task timers.

| Concurrent group | Pairs | Direct Git median / p90 | Aethyme median / p90 |
| --- | ---: | ---: | ---: |
| Six independent changes | 6 | 53.81 / 66.19 s | 96.48 / 116.97 s |
| Textual overlap | 2 | 58.88 / 78.15 s | 108.20 / 144.54 s |
| Semantic overlap | 2 | 49.05 / 51.45 s | 62.45 / 70.38 s |
| Harmless shared-file changes | 2 | 47.64 / 51.27 s | 90.78 / 99.38 s |

### Command attribution

Broker log values are milliseconds. Quantiles describe each command's
own distribution. Summed command time can overlap across workers, and
the attribution buckets contain the union of observed spans. The release
did not assign the gate child to the child-command bucket, so the
residual for submit includes most of the gate execution.

| Broker command | Calls | Total | Wall median / p90 | Child median / p90 | Queue median / p90 | Residual median / p90 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| submit | 12 | 145,661 | 11,417 / 20,084 | 3,019.5 / 4,705 | 5.5 / 10 | 7,845.5 / 14,645 |
| finish | 12 | 27,854 | 2,290.5 / 2,559 | 1,821 / 1,956 | 0 / 7 | 421.5 / 758 |
| advance | 13 | 22,697 | 1,683 / 2,182 | 1,097 / 1,516 | 20 / 52 | 477 / 875 |
| start | 12 | 19,070 | 1,429 / 1,742 | 1,023.5 / 1,309 | 4 / 10 | 223 / 833 |
| hook.PreToolUse | 96 | 17,804 | 118 / 385 | 97.5 / 235 | 0 / 0 | 10.5 / 134 |
| hook.PostToolUse | 96 | 16,375 | 109.5 / 281 | 100.5 / 263 | 0 / 0 | 10 / 39 |
| hook.SessionStart | 13 | 14,196 | 1,047 / 1,357 | 697 / 1,033 | 0 / 0 | 346 / 443 |
| hook.Stop | 13 | 10,365 | 750 / 1,012 | 652 / 852 | 0 / 0 | 80 / 120 |
| hook.UserPromptSubmit | 13 | 8,022 | 630 / 761 | 512 / 662 | 0 / 0 | 67 / 153 |
| status | 6 | 3,113 | 442 / 878 | 267 / 505 | 0 / 0 | 211 / 390 |

The gate itself took 6.45 / 11.46 s in the broker. Running the same suite
directly took 4.77 / 6.85 s. Arm order was balanced, but the shared 10-CPU
host was busy and the measured suite runtime varied by arm. The elapsed
difference cannot all be attributed to broker bookkeeping. The five
stock hook commands consumed 66.8 s of summed command time across the
12 assignments and one repair. Pre- and post-tool hooks account for
34.2 s of that total, mostly Git child time. Their median recorded output
was zero bytes. The intervention and rework counts do not show an offset
for those costs in this cohort. A larger trial across multiple hosts and
real workloads would be needed to generalize the magnitude.
