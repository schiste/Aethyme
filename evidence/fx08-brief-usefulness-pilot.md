# Local v3 FX08 brief usefulness pilot — v14 behavior-oracle-v2 frozen rerun

Frozen revision 694ad1e4bf71eb40e669e0a8557dec1039bd73b1. This fixed-file handoff pilot does not close D30 because it does not use L3 retrieval.

| Variant | Agent runs / attempts | Decision survived | Task passed | HANDOFF read | Median wall time | Median uncached + output tokens |
|---|---:|---:|---:|---:|---:|---:|
| absent | 12/12 | 100.0% | 91.7% | 100.0% | 36.7s | 23439 |
| correct | 12/12 | 100.0% | 100.0% | 100.0% | 38.3s | 12836.5 |
| incomplete | 12/12 | 100.0% | 100.0% | 100.0% | 39.8s | 13388.5 |
| misleading | 12/12 | 100.0% | 91.7% | 100.0% | 40.4s | 13292 |

## Task-level outcomes

| Task | Agent runs | Decision survived | Task passed | HANDOFF read |
|---|---:|---:|---:|---:|
| P01 | 12/12 | 100.0% | 100.0% | 100.0% |
| P02 | 12/12 | 100.0% | 83.3% | 100.0% |
| P03 | 12/12 | 100.0% | 100.0% | 100.0% |
| P04 | 12/12 | 100.0% | 100.0% | 100.0% |

## Observed patterns

- Task completion ranged from 83.3% (P02) to 100.0% (P01, P03, P04); with three repeats per task-condition, these are descriptive counts, not significance claims.
- Observed task-pass difference for correct versus absent: +8.3 percentage points; this small pilot is descriptive and does not establish causation.
- Observed task-pass difference for incomplete versus absent: +8.3 percentage points; this small pilot is descriptive and does not establish causation.
- Observed task-pass difference for misleading versus absent: +0.0 percentage points; this small pilot is descriptive and does not establish causation.
- No decision break was observed in 12 misleading-brief runs.
- The runner leakage gate returned code 3 for 8 runs; leakage details are retained per run.


## What stood out

- Correct and incomplete briefs produced the same aggregate result: 12/12 task passes and 12/12 decisions survived in each condition. This pilot found no measured benefit from supplying the omitted decision content over the incomplete brief.
- Both task failures were P02 narrow-layout failures: at a 375px viewport, the document widths were 392px and 409px. The controls themselves did not overlap; both final messages said browser layout testing was unavailable to the agent.
- The absent condition had a higher median uncached-plus-output cost (23,439 tokens) than the correct condition (12,836.5), despite its shorter handoff. With three repeats per task-condition and no model identifier in the run events, this is an observed difference only.
- All 48 agents read HANDOFF.md and all 48 decision checks passed. None of the 12 misleading briefs broke the decision.
- Eight runs received exit code 3 after the runner leak gate found generated artifacts in command output: seven .codex markers and one AGENTS.md marker, all from external paths. Their agents had started and their behavior scores and raw artifacts are retained; the batch driver therefore exited 1 despite completing all 48 rows.

## Protocol limits

- H01 and H02 were excluded because they and prior outcomes were exposed; 8 held-out runs are missing.
- The v14 freeze precedes this rerun, but not the earlier agent runs. It is not a first-ever freeze; v13 and earlier evidence remains preserved, and v13 scores are superseded because its oracle missed written behaviors.
- Model identifiers are recorded only if present in each event log; the runner does not otherwise expose them.
- Raw run artifacts are retained outside this Playground repository under the per-run paths in the JSON.
- This is a fixed-file pilot, not a run through L3 retrieval, so it cannot close D30.
- The no-brief condition keeps the required exact text, so it is inherently much shorter than the three actual decision briefs. Correct, incomplete and misleading brief token counts are recorded in validation/brief-validation.json; length remains a condition-level limitation.
- The runner inherits the same host CODEX_HOME for all arms while ignoring user config. Any generated-artifact leakage is reported per run; these outcomes are retained rather than filtered.

## Misleading-brief harm

Decision break observed in 0 of 12 misleading-brief runs.


## Frozen package integrity follow-up (v15)

A post-run audit found that the six v14 handoff framing sidecars retained the earlier digest 39cb1e70dbb145abceece6c949dc97142358c342abe428d86e8f68306d789423, while the actual shared framing and the v14 pre-freeze check produce 1f92fc7b2f2f1e1e7a74d15a63d63630fce59a216b27d8c3c39404602c2090c1. The 48 HANDOFF.md copies used in the pilot match the v14 frozen source byte-for-byte.

The separate v15 freeze corrects only those six sidecars, its README, pre-freeze record, and manifest. Task content, briefs, prompts, oracles, browser settings, and pilot configuration are unchanged. V15 is frozen at commit b684ea6cbcf2c248cdc7cc04fbd8bea4065ca718 (tag fx08-v15-frozen; bundle SHA-256 784492acd0b051c8972e4bae84d1cf02b483c1243c5e0d6906b010efd80ae37e) and has no agent runs. The pilot results and all 48 raw artifact sets remain tied to v14 revision 694ad1e4bf71eb40e669e0a8557dec1039bd73b1; they were not rescored. The held-out shortfall remains eight runs.
