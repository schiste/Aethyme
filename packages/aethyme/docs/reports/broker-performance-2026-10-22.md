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

Status and sync medians are within budget; individual aged status samples
exceeded 500 ms. Guard and push medians exceed theirs.
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
runs succeeded. The retained measurement archive has 22 payload files (186,416
bytes), plus a 3,199-byte copy manifest listing their hashes and sizes.

Router SHA-256 identities:

- Baseline: `799e297b562dfe73be220d500ee8b71f18bc95adf85b42fc733aea30a5bb5321`.
- Candidate: `6a06ba0ba4bbfa62b852b7ebc5b5c921539c2974fcf13a9b1802196b7b2facea`.

Next, isolate supervisor startup and cleanup costs, then remove redundant
command-local work without weakening lease, compatibility, ownership or
post-command checks. Repeat the same fresh and aged workloads with matched
release-profile binaries after the combined fixes are qualified. No new timing
budget, cleanup exemption or safety bypass is introduced by this report.

## Follow-up: combined candidate and readiness repair

The combined candidate at `0e0a9b2025594d5e647518c6ad4feeeb1acd165e`
includes the readiness-loop repair and the qualified Local fixes through #972
and #973. Its router and engine built successfully together; 17 release-contract
and 10 gate-cache tests passed locally. It is a development candidate, with no
installed-runtime change at measurement time.

This comparison uses two development pairs with debug information and
incremental compilation disabled: the earlier `4acd3cec` candidate above and
the combined `0e0a9b20` candidate. The same harness and local Git transport were
used for both. Fresh measurements ran the combined candidate first, with three
samples per action. Aged measurements used 1,000 released host-resource leases
and both binary orders, with six pooled samples per action. Each fixture had
one broker session. The host was shared with other work; these are diagnostic
CLI timings, not isolated-host or production tail-latency qualification.

| Action | Fresh earlier | Fresh combined | Aged earlier | Aged combined | Aged change |
| --- | ---: | ---: | ---: | ---: | ---: |
| Summary status | 447.6 | 450.8 | 484.7 | 420.3 | -13.3% |
| Guarded no-op | 1736.0 | 899.5 | 1550.3 | 816.9 | -47.3% |
| Unchanged sync | 830.5 | 741.4 | 694.5 | 786.2 | +13.2% |
| Unchanged push | 1387.0 | 1123.7 | 1089.8 | 1144.5 | +5.0% |

All values are milliseconds. Guarded execution improved in both workloads,
consistent with removing a wait after every already-available readiness byte.
A deterministic regression control restored that wait and failed with 33 waits
instead of zero; after exact restoration, the same test passed. This isolates
the readiness defect without attributing every timing difference to that fix.

The aged sync median regressed and must remain visible. Its combined samples
ranged from 531.6 to 1054.7 ms; push ranged from 757.3 to 1380.8 ms. Guard and
push remain above their 500 ms warning budgets. No budget was relaxed.
The six benchmark runs completed successfully: 22 commands per fresh run and
28 per aged run. Raw samples, command outputs, workload checks, and router and
engine hashes before and after execution are retained with the measurements.
The counterbalanced summary SHA-256 is
`cd0cce2acf46ee0a518dd8fde8af740f6b2f88d2ae744e0a7764573a06989a63`.

Combined pair SHA-256 identities:

- Router: `70eba47ebccbe9c2fa463b9df306cbdcaf818bc1eb1fc8b1ae8d03f21e8c9ab9`.
- Engine: `9146e8c7eca98f0cb18d9c687d8a296eb4dd622af21fbab028be6188c59fff93`.

Next measurements should cover many sessions and worktrees separately from
released resource-lease history, and repeat the published release pair before
claiming installed-version performance. Reducing command-local Git discovery
and config reads remains a candidate improvement; the sync result needs
investigation rather than a timing exemption.


## Published release comparison: v0.8.28 to v0.8.29

After publication, the official router and engine pairs were measured with the
same frozen harness, budgets and 16-file synthetic fixture. The four runs were
sequential in this order: v0.8.28 fresh, v0.8.29 fresh, v0.8.29 aged, v0.8.28 aged.
Each run used one session and three samples per action. The aged profile seeded
1,000 released host-resource leases; it did not create 1,000 sessions. All four
runs and their workload checks passed. The campaign completed in 43.941 seconds.

The published v0.8.29 pair is now installed globally and selected by the Aethyme
repository runtime. Its release commit is
`2c78fd1bcd2f7427e8022f6c0778f65b6be14661`. Later changes, including #979 and
#982, are absent from these binaries and these measurements.

Milliseconds; each cell lists all three complete CLI samples followed by the
median in parentheses. Changes compare medians; budgets remain warnings.

| Profile | Action | v0.8.28 samples (median) | v0.8.29 samples (median) | Change |
| --- | --- | ---: | ---: | ---: |
| Fresh | Summary status | 278.8, 298.9, 312.9 (298.9) | 337.9, 319.7, 310.2 (319.7) | +7.0% |
| Fresh | Guarded no-op | 1450.0, 1686.4, 1535.9 (1535.9) | 720.4, 682.2, 720.9 (720.4) | -53.1% |
| Fresh | Unchanged sync | 580.7, 695.0, 617.9 (617.9) | 503.4, 486.8, 427.5 (486.8) | -21.2% |
| Fresh | Unchanged push | 951.7, 1033.5, 936.9 (951.7) | 745.4, 674.2, 673.8 (674.2) | -29.2% |
| Aged | Summary status | 303.8, 300.8, 291.2 (300.8) | 377.3, 341.5, 283.8 (341.5) | +13.5% |
| Aged | Guarded no-op | 1513.8, 1603.8, 1323.0 (1513.8) | 796.3, 681.4, 712.7 (712.7) | -52.9% |
| Aged | Unchanged sync | 632.8, 535.8, 570.9 (570.9) | 478.9, 514.4, 526.4 (514.4) | -9.9% |
| Aged | Unchanged push | 1064.0, 953.6, 1125.9 (1064.0) | 671.1, 674.2, 664.1 (671.1) | -36.9% |

Guard and push improved in both workloads but still exceed their 500 ms median
warning budgets. Status medians increased in both workloads, while remaining
within 500 ms. Sync remains within its 1,000 ms budget. Three samples on a shared
host do not establish statistical significance or production tail latency.
Start and submit are not timed by this workload. Push uses an unchanged branch
and local SSH transport to a bare remote, not a public-network delivery. These
results measure neither model token cost nor concurrent-session scaling.

The 48 budgeted samples and six additional aged resource-list samples have
complete command/output receipts. Before/after hashes and version banners
matched for both binary pairs. All evaluations ran on isolated Playgrounds.
One earlier runner setup failed at JSON serialization before any benchmark child
started; it is retained separately and contributes no samples.

Reproduction pins (SHA-256):

- Harness: `282c6219fc21d2062f0e611c865073c90e1d670f5ac74876434c29f0e5b7fefe`.
- Budgets: `c78d9132690ac663a787738cf74a500d0df76799e60b3f7ae5fe33ef1c3b94b4`.
- Frozen fixture inventory: `e639ab047299a19ef9e1c7ea45beb4c98c63b69bdfd26b41bbc33b5d1671e0de`.
- v0.8.28 router: `799e297b562dfe73be220d500ee8b71f18bc95adf85b42fc733aea30a5bb5321`.
- v0.8.28 engine: `95e818136304380ff7e46dc23d7e9652b5eb0f77b02f67a4cfa0ea11d95f9011`.
- v0.8.29 router: `0349515fb3d9b7387b038d1a5d525fac56d35a8302cfa53be7d5c91b316bbbb0`.
- v0.8.29 engine: `dd723d77eeeae1e6664e8f1b2320a7c5b4c6d9260575c4086ae74675f0d8564f`.
- Campaign receipt: `073fd65dbca966fc33ccfab5d4a4d3cfd9b954bd255a02dd213d2f36b19ca559`.
- Matched summary JSON: `3f1e064f6b4405f92da7823f297f05e061c976cd33cbe2bc908975ab705ba405`.

The next performance work should investigate the status increase, bring guarded
execution and unchanged push under their existing budgets, and measure many
sessions separately from released resource-lease history. The earlier debug
comparisons remain historical diagnostics and are not pooled with this release
comparison.
