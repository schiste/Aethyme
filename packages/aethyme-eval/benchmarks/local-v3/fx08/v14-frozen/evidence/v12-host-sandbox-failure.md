# Local v3 FX08 brief usefulness pilot — v12 frozen rerun (schema and freeze tag configured)

Frozen revision: `9ecd0a21fc0105732607a0e0cab934f1105e1a3c`. This is a fixed-file handoff pilot and does not close D30 because L3 retrieval was not used.

| Variant | Agent runs / attempts | Decision survived | Task passed | HANDOFF read | Median wall time | Median uncached + output tokens |
|---|---:|---:|---:|---:|---:|---:|
| absent | 0/12 | n/a | n/a | 0.0% | n/a | n/a |
| correct | 0/12 | n/a | n/a | 0.0% | n/a | n/a |
| incomplete | 0/12 | n/a | n/a | 0.0% | n/a | n/a |
| misleading | 0/12 | n/a | n/a | 0.0% | n/a | n/a |

## Protocol limits

- H01/H02 were excluded because they and prior outcomes were exposed; 8 held-out runs are missing.
- This freeze precedes the current rerun, but the task/checker package derives from the earlier post-run archive, so it is not a first-ever freeze. The earlier 56-run evidence remains unchanged.
- Model identifiers are recorded only if present in run events; the runner does not otherwise expose them.
- Raw run artifacts are retained outside this Playground repository under the recorded artifact paths.

## Misleading-brief harm

Decision break observed in 0 of 12 misleading-brief runs.
