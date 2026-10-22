# Broker performance: 2026-10-22

Last Updated: 2026-10-22

## Scope and method

Three complete CLI wall-time samples per action, on fresh isolated synthetic
Playgrounds and on Playgrounds seeded with 1,000 released host-resource leases.
Push uses an offline bare remote through a local SSH transport stub; these are
not network-delivery timings. No evaluation ran against Aethyme itself.

The baseline is the official v0.8.28 router. The candidate is a matched
v0.8.29 development pair built at `4acd3cec2c654f8a2b44a043ba4fdd8b41f702ce`
with debug information and incremental compilation disabled. Different build
profiles prevent attributing differences solely to source changes. Neither this
candidate nor the later merged Git fixes have been installed by this sweep.

## Complete CLI medians

Milliseconds; budgets remain warnings, as defined in
`packages/aethyme-eval/benchmarks/performance/broker-budgets.json`.

| Action | Budget | Fresh baseline | Fresh candidate | Aged baseline | Aged candidate |
| --- | ---: | ---: | ---: | ---: | ---: |
| Summary status | 500 | 339.8 | 359.4 | 383.0 | 420.8 |
| Guarded no-op | 500 | 1189.6 | 1307.5 | 1239.7 | 1335.7 |
| Unchanged sync | 1000 | 629.3 | 632.3 | 621.8 | 624.1 |
| Unchanged push | 500 | 1349.6 | 839.4 | 1325.7 | 769.5 |

Status and sync meet their budgets in these samples. Guard and push miss theirs.
Push is lower in both workloads, but this is not a release-profile speedup claim.
Three samples are diagnostic evidence, not a tail-latency qualification.

## Additional instrumented diagnostic

A separate one-sample fresh candidate run used Git Trace2. These instrumented
wall times are excluded from the median comparison above. Actual Git start
records, including descendants, counted 31 launches for status, 50 for guarded
execution, 46 for sync, and 70 for push. Repeated repository discovery and
committed-config reads account for many launches.

For guarded execution, summed Git-reported lifetimes were about 78 ms within
1,426 ms of instrumented complete CLI time. These lifetimes exclude some
process-launch overhead and may overlap; they are not a full attribution of
wall time. An approximately 840 ms gap before the post-command status check
points to the supervised-execution phase for further investigation. It does
not establish which startup, command, or cleanup step caused the delay.

Separate process sampling observed startup GC and worktree-root/config
resolution. Sampled stack counts are not invocation counts. A further process
observation retained only the diagnostic broker's own descendant identities;
observer overhead makes those timings unsuitable for the benchmark table.

## Reproducibility and remaining work

The existing `broker_latency.py` harness produced complete commands, outputs,
raw samples, workload descriptions and binary identities. All four benchmark
runs succeeded. The retained measurement archive contains 22 files (186,416
bytes); its copy manifest verifies the evidence independently of build output.

Router SHA-256 identities:

- Baseline: `799e297b562dfe73be220d500ee8b71f18bc95adf85b42fc733aea30a5bb5321`.
- Candidate: `6a06ba0ba4bbfa62b852b7ebc5b5c921539c2974fcf13a9b1802196b7b2facea`.

Next, isolate supervisor startup and cleanup costs, then remove redundant
command-local work without weakening lease, compatibility, ownership or
post-command checks. Repeat the same fresh and aged workloads with matched
release-profile binaries after the combined fixes are qualified. No new timing
budget, cleanup exemption or safety bypass is introduced by this report.
