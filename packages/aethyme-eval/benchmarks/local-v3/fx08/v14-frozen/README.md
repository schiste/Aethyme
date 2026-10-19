# FX08 brief usefulness pilot — frozen rerun package (v14)

This disposable Playground repository contains four pilot tasks (P01–P04), two previously exposed holdout tasks (H01–H02), their behavior oracles, and four HANDOFF variants per task. It contains synthetic UI code only and is not an Aethyme evaluation target.

The six task descriptions were approved before fixture construction. The source tasks, briefs, and prompts remain unchanged from v13. The v13 result set and raw runs are preserved under the v13 archive entries; a source audit found that its checker did not cover all written behaviors. V14 updates the P01–P04 oracle requirements/checker, run harness metadata, and stale framing digests while reusing the approved task content and unchanged brief variants. It is a new oracle version, not a post-hoc rescore and not tuned to agent output.

The original 56-run series predates the first archive freeze; its prompts were reconstructed from approved summaries and traces, and its checker was repaired after those runs. V14 is frozen before its rerun but cannot retroactively satisfy first-ever freeze ordering. The v10, v11, v12 and v13 results remain unchanged.

P01–P04 now use Playwright browser behavior for the accessibility, form, filter, keyboard and responsive checks. The local Playwright module is pinned in `pilot-config.json` and Chrome is launched headlessly against a loopback server serving each disposable repo. The CLI wrapper was unavailable, so the checker uses the installed Playwright Node API. The runner remains `run_codex_eval.py`, and the Codex child remains `--sandbox workspace-write`; the outer controller is launched with the recorded host permission needed for its local app-server.

H01 and H02 are excluded from held-out runs because they and prior outcomes were already exposed. They remain in the package for provenance; no v14 held-out results will be claimed, leaving 8 held-out runs as an explicit shortfall.

`validation/brief-validation.json` records that all 24 brief files passed `cargo run -q -p aethyme-contracts --example check_brief -- <path>` against the isolated PR #708 source snapshot. Brief bytes are unchanged.

The v14 tag freezes tasks, briefs, prompts, oracles, browser dependency settings, run configuration, and digests before any v14 agent run. Do not alter them after the tag. Evidence is added after the freeze; raw Codex artifacts remain outside this repository.

This pilot delivers the brief as a fixed `HANDOFF.md`, not through L3 retrieval. It does not close D30.


Before the v14 freeze, every P01–P04 browser oracle was exercised on its A-state, a separate synthetic behavior-success control, and a separate control that deliberately violates the decision. Those controls are outside the task and run repositories; see validation/v14-oracle-smoke.json. The oracle reads only the written behavioral requirements and rendered browser state, not an expected source file.

All 24 brief files passed the required cargo check in the isolated PR #708 source snapshot, as recorded in validation/brief-validation.json. V14 confirms their bytes are unchanged. Correct, incomplete and misleading variants are similar in token count per task. The absent condition must retain the exact required sentence and is necessarily shorter; this residual length confound is reported rather than padded with a fake brief.
