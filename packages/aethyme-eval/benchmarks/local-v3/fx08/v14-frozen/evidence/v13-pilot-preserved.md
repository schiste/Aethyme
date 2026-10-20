# Local v3 FX08 brief usefulness pilot — v13 frozen rerun (isolated roots and host launcher)

Frozen revision: `c25228747d3c64f0482aae5969dbba56a851fe9a`. This is a fixed-file handoff pilot and does not close D30 because L3 retrieval was not used.

| Variant | Agent runs / attempts | Decision survived | Task passed | HANDOFF read | Median wall time | Median uncached + output tokens |
|---|---:|---:|---:|---:|---:|---:|
| absent | 12/12 | 100.0% | 50.0% | 100.0% | 43.9s | 17568 |
| correct | 12/12 | 100.0% | 50.0% | 100.0% | 41.7s | 20773 |
| incomplete | 12/12 | 100.0% | 50.0% | 100.0% | 40.8s | 15450 |
| misleading | 12/12 | 100.0% | 50.0% | 100.0% | 52.0s | 24630 |

## Protocol limits

- H01/H02 were excluded because they and prior outcomes were exposed; 8 held-out runs are missing.
- This freeze precedes the current rerun, but the task/checker package derives from the earlier post-run archive, so it is not a first-ever freeze. The earlier 56-run evidence remains unchanged.
- Model identifiers are recorded only if present in run events; the runner does not otherwise expose them.
- Raw run artifacts are retained outside this Playground repository under the recorded artifact paths.

## Misleading-brief harm

Decision break observed in 0 of 12 misleading-brief runs.
