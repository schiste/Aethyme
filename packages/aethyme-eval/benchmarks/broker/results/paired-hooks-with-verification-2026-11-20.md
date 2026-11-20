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
12 assignments and one repair. They recorded 4,566 Git subprocess
spans. Pre- and post-tool hooks account for 34.2 s and 2,229 of those
spans, with nine Git spans at the median for either boundary. Their
median recorded output was zero bytes. The intervention and rework
counts do not show an offset for those costs in this cohort. A larger trial across multiple hosts and
real workloads would be needed to generalize the magnitude.

### Six-way cheap-gate control

A separate run on the same release and model repeated the six independent
assignments concurrently. Both arms completed all six tasks and passed
all six exact-tree checks. Each checked a different integration tree,
with zero reuse, conflicts, repairs, or human interventions. Trusted
hooks recorded 18 activity signals.

| Measure | Direct Git median / p90 | Aethyme median / p90 |
| --- | ---: | ---: |
| Task duration | 44.83 / 45.82 s | 61.90 / 74.18 s |
| Uncached input tokens | 8,441.5 / 9,875 | 7,330.5 / 8,947 |
| Cached input tokens | 105,728 / 149,504 | 99,968 / 127,744 |
| Output tokens | 1,176 / 1,511 | 1,130.5 / 1,378 |
| Exact-tree verification wall time | 64 / 81 ms | 2,890 / 3,382 ms |

Aethyme took 38.1% longer at the median and 61.9% longer at p90.
The median within-pair duration difference was +17.51 s.
The broker used 13.2% fewer uncached input tokens at the median. The
small token sample and different direction in the substantial-gate
cohort show sensitivity to task mix and agent behavior. An audit found one
failed broker start by the direct worker for t10. Excluding that pair,
median task duration was 44.60 s direct and 59.87 s broker, and median
uncached input was 8,298 versus 7,966 tokens. The remaining five tasks
ran in the same six-worker group, so this is a sensitivity check.

## Opt-in early verification

[PR #1883](https://github.com/schiste/Aethyme/pull/1883) starts bounded
background verification after publication. Passing results are eligible
for reuse when ordinary submit independently proves the same candidate
tree, gate definition, and execution profile. The worker has one
repository slot, yields at busy admission checks, and interrupts its own
gate when the local head or fetched base changes.

The controlled run used the same development build
(`68d2916c5498ffa6dbdb2e806a8381797d34eb7e`) in both arms. Each pair used
fresh identical Playground fixtures. One arm published with ordinary
`advance` and the other with `advance --preverify`. Scenarios paired a
one-pass gate with leads of 0 or 3 seconds, and a 40-pass gate with leads
of 1 or 7 seconds. Each scenario ran twice with the arm order reversed.

| Gate | Pairs | Ordinary submit median / p90 | Early submit median / p90 | Median paired time saved in submit | Median paired time saved across publish and submit |
| --- | ---: | ---: | ---: | ---: | ---: |
| All | 8 | 9.38 / 24.98 s | 6.62 / 19.55 s | 2.67 s | 2.39 s |
| One pass | 4 | 6.23 / 8.53 s | 4.71 / 6.47 s | 1.77 s | 0.74 s |
| Forty passes | 4 | 17.37 / 24.98 s | 10.26 / 19.55 s | 4.01 s | 3.65 s |

All eight ordinary and eight early submits passed, and every early submit
reused the background worker's exact verdict. Zero workers had finished
before submit started in this run. Submit joined the work in progress or
read its result after normal setup. Seven of eight submits were faster.
Median publication took 2.30 s ordinary and 2.67 s with the worker. The
eight independently executed foreground gates consumed 46.4 s in total,
and the eight early gates consumed 48.2 s. Early execution moved work off
the critical path. On this busy host, the cheap gate's measured latency
change was larger than the gate itself, so the estimates still include
host and command scheduling variation.

Two deliberate supersession cases interrupted a running gate while
another worker was refused the occupied slot. The worker exited 0.91 s
after the local head changed and 1.74 s after the fetched base changed.
The cancelled gates had already spent 1.28 s and 1.79 s on their commands
and left no reusable verdict. An earlier build exposed a cache persistence
assertion on cancellation. The repaired build passed both probes and the
[hosted cancellation-and-reuse regression](https://github.com/schiste/Aethyme/actions/runs/44766634129).

### Development build in the live comparison

The same development build ran the 12 assignments with trusted hooks
and `--preverify`. All assignments and exact-tree checks passed in
both arms. Each arm again repaired one textual conflict and recorded
zero human interventions. This run compared the candidate with direct
Git. The controlled on/off run above isolates the opt-in switch more
closely.

| Measure | Direct Git median / p90 | Early Aethyme median / p90 | Change |
| --- | ---: | ---: | ---: |
| Task duration | 53.23 / 68.46 s | 68.50 / 85.10 s | +28.7% / +24.3% |
| Uncached input tokens | 8,187.5 / 17,574 | 10,255 / 18,277 | +25.3% / +4.0% |
| Cached input tokens | 97,408 / 153,344 | 115,456 / 293,888 | +18.5% / +91.7% |
| Output tokens | 1,348 / 1,674 | 1,132 / 1,840 | −16.0% / +9.9% |
| Exact-tree verification wall time | 3.78 / 5.53 s | 6.07 / 6.97 s | +60.4% / +26.0% |

The median paired task-duration difference was +10.86 s. The broker
was slower in all 12 pairs. In the six-way group, task time was
57.64 / 68.46 s direct and 68.50 / 85.10 s broker. The release and
development runs took place at different times and produced different
direct controls, so their difference cannot isolate the feature.

Of ten scheduled workers, three yielded to the repository slot holder,
one was superseded, one produced no verdict, and five passed a gate.
All ten produced telemetry. Both textual tasks and one semantic task
reused an early verdict, so 3 of 12 final submits skipped physical gate
execution. These verdicts covered 11.30 s of gate work. The two remaining
gate runs consumed 6.29 s without a final reuse. Both came from the
six-way group. The integration base advanced between landings in that group.
The broker executed 14 gates across both stages, versus 12 in direct Git.
The workers that yielded or skipped ran no gate commands. Successful
early execution moved three checks off the foreground path and increased
total executions by two.

| Broker command | Calls | Total (ms) | Wall median / p90 (ms) | Child median / p90 (ms) | Queue median / p90 (ms) | Residual median / p90 (ms) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| submit | 12 | 72,347 | 6,044.5 / 6,938 | 5,180 / 6,311 | 1 / 2,672 | 430 / 639 |
| submit preverify | 10 | 35,058 | 3,549 / 6,208 | 3,239.5 / 5,792 | 0 / 3 | 359.5 / 413 |
| finish | 12 | 20,212 | 1,621 / 2,173 | 1,270 / 1,801 | 0 / 1 | 298 / 506 |
| start | 12 | 16,951 | 1,145.5 / 2,245 | 973.5 / 1,206 | 0 / 255 | 157 / 445 |
| advance | 13 | 16,052 | 1,246 / 1,480 | 833 / 933 | 13 / 35 | 300 / 612 |
| hook.PreToolUse | 98 | 10,745 | 81 / 183 | 75 / 140 | 0 / 0 | 7.5 / 53 |
| hook.PostToolUse | 97 | 14,186 | 82 / 276 | 73 / 254 | 0 / 0 | 9 / 24 |

The development build assigns gate subprocess time to the child bucket.
The combined command log labels both operations `submit`. The table
splits out the rows that carry early-verification telemetry.

## Published v0.8.141 with trusted hooks

The current-release comparison used the checksum-verified v0.8.141 binary
(`2093b098d9cfc0a51ffadd2778ed4637a9c096b1`), the same Codex 0.170.0
model settings, seed, and substantial gate as the previous release.
Both arms completed all 12 assignments and passed all 12 exact-tree
checks. Neither arm reused gate work or needed a human intervention.
The coordinator detected and repaired one textual conflict in each arm.

| Measure | Direct Git median / p90 | Aethyme median / p90 | Change |
| --- | ---: | ---: | ---: |
| Task duration | 50.90 / 72.09 s | 59.53 / 71.64 s | +17.0% / −0.6% |
| Uncached input tokens | 8,436.5 / 12,997 | 8,310 / 12,603 | −1.5% / −3.0% |
| Cached input tokens | 106,240 / 117,632 | 109,248 / 240,000 | +2.8% / +104.0% |
| Output tokens | 1,170 / 1,724 | 1,205 / 1,693 | +3.0% / −1.8% |
| Exact-tree verification wall time | 3.58 / 4.62 s | 5.28 / 6.28 s | +47.5% / +35.9% |

The paired median duration difference was +7.32 s. All six wins came
from the six independent assignments. The other six needed more time.
The p90 advantage was about half a second and is sensitive to the small
sample. Input token medians and paired differences answer different
questions here. The paired median change was +657 uncached tokens.

| Concurrent group | Pairs | Direct Git median / p90 | Aethyme median / p90 | Median paired difference |
| --- | ---: | ---: | ---: | ---: |
| Six independent changes | 6 | 61.41 / 72.09 s | 58.69 / 71.64 s | −2.47 s |
| Textual overlap | 2 | 60.44 / 85.32 s | 84.03 / 104.22 s | +23.59 s |
| Semantic overlap | 2 | 34.83 / 38.13 s | 50.81 / 54.97 s | +15.97 s |
| Harmless shared-file changes | 2 | 40.52 / 42.86 s | 62.17 / 66.44 s | +21.65 s |

The table shows the full assigned cohort. During the shared-file group
one direct worker attempted a broker start, which failed before a session
opened and used 421 ms of broker command time. Its model turn can also
have changed. Excluding the whole affected group, task medians were
56.30 s direct and 58.69 s broker, and the median paired difference was
−0.74 s across the ten remaining pairs. This checks sensitivity to the
protocol deviation as well as to dropping that task group. The harness
now gives the direct arm explicit Git-only coordination instructions,
keeps the measured broker binary off that arm's injected PATH, and
rejects the group if it observes broker command telemetry.

### Command and completed-task attribution

All rows below are from the broker arm. They include any repeated
calls within that arm. The gate command itself took 2.68 / 3.26 s;
the older release's gate took 6.45 / 11.46 s. Host load, task behavior,
and gate runtime varied across release runs. Those factors prevent
assigning the aggregate release difference to a single code change.

| Broker command | Calls | Total (ms) | Wall median / p90 (ms) | Child median / p90 (ms) | Queue median / p90 (ms) | Residual median / p90 (ms) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| submit | 12 | 63,886 | 5,261.5 / 6,243 | 4,955 / 5,763 | 1 / 4 | 364 / 470 |
| finish | 12 | 16,130 | 1,164 / 1,755 | 986.5 / 1,526 | 0 / 0 | 177.5 / 229 |
| advance | 13 | 12,600 | 969 / 1,066 | 723 / 804 | 13 / 19 | 210 / 265 |
| start | 12 | 10,735 | 900 / 1,122 | 731.5 / 895 | 0.5 / 5 | 88 / 481 |
| hook.PostToolUse | 99 | 9,547 | 67 / 123 | 63 / 100 | 0 / 0 | 4 / 10 |
| hook.SessionStart | 13 | 9,273 | 678 / 977 | 513 / 793 | 0 / 0 | 169 / 213 |
| hook.PreToolUse | 99 | 8,996 | 69 / 157 | 66 / 126 | 0 / 0 | 4 / 44 |
| hook.Stop | 13 | 6,270 | 430 / 668 | 399 / 540 | 0 / 0 | 26 / 119 |
| hook.UserPromptSubmit | 13 | 5,913 | 447 / 601 | 421 / 539 | 0 / 0 | 29 / 34 |
| status | 4 | 1,582 | 403.5 / 539 | 201.5 / 312 | 0 / 0 | 186.5 / 258 |

The broker's completed-task reports attributed 297 commands to 12
tasks. Per task they recorded 11.46 / 15.52 s of summed command time,
10.28 / 13.06 s of child work, 18 / 27 ms of queue time, and
1.46 / 2.41 s of residual time. Recorded output was 14,670 / 22,362
bytes per task. Bytes measure command output and use a separate source
from the actual model token counts above. Broker telemetry recorded no
retries, gate sharing, or duplicate work avoided. The textual repair
counts came from the coordinator's Git check before submit, so broker
conflict event counts remained zero.

Pre- and post-tool hooks recorded 18.54 s of summed time and 2,288 Git
subprocess spans over 198 boundaries. Each boundary type again had a
median of nine Git spans and zero output bytes. This is repeated work
even when an agent receives no update. The measured next optimization
target is unchanged tool boundaries, where skipping repeated Git
discovery can remove work before testing for task-relevant changes.
