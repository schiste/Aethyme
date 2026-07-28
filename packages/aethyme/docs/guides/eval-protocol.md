# Eval Protocol (removed)

Last Updated: 2026-07-28

The evaluation harness (`src/eval/`, `evals/tools/*.toml`, the
`aethyme eval` / `aethyme methodology` CLI groups, and
`packages/aethyme-eval-ui/`) was removed on 2026-07-13 as part of the
local-first broker repositioning. The full protocol this document
described (5+1 condition matrix, playground setup, Chau7 execution,
scoring, methodology fingerprinting) is preserved in distilled form at
[`../architecture/eval-mining-notes.md`](../architecture/eval-mining-notes.md).

The Cardinal Rules in the repository root `CLAUDE.md` still apply to any
future evaluation work: evals run only against Playground repositories,
and nothing is ever tuned to improve an eval score. The audit trail of
rejected eval-tuning proposals remains at
[`../architecture/eval-tuning-rejected.md`](../architecture/eval-tuning-rejected.md).

## Live Playground A/B Contract

The remaining live eval surface is the playground A/B runner under
`scripts/eval/`. Any Control vs Aethyme comparison must hold this contract:

- The target repository is a Playground clone, never the Aethyme checkout.
- The Control arm has no `.aethyme/`, `.chau7/`, `.codex/`, `.claude/`,
  generated `AGENTS.md`, generated `CLAUDE.md`, graph store, skill, or
  Aethyme environment leakage.
- The Aethyme arm exposes only the intended enhancement surface: generated
  root guidance, the Aethyme skill/reference files, fragment graph, and local
  Redb graph store. Internal eval skills are forbidden.
- Both arms use the same Codex runner settings. The bundled runner uses
  `codex exec --ignore-user-config --json` by default so global MCP servers,
  hooks, and local user config do not become the measured variable.
- Runs preserve `events.jsonl`, `stderr.log`, `last-message.json`,
  `command.json`, and `contract.json`, and emit wall time, token usage,
  command-output chars, event-log chars, and stderr chars in the runner JSON.
- `.aethyme` path leakage in selected files, snippets, command output, or the
  final answer is a hard regression. The bundled runner writes `leakage.json`
  and exits non-zero when the leak gate trips.

Set `AETHYME_EVAL_ARM=control` or `AETHYME_EVAL_ARM=aethyme` explicitly when
using `scripts/eval/run_codex_eval.py`. For reproducible archives, set
`AETHYME_EVAL_ARTIFACT_DIR` to a run-specific directory outside the target
repository.

Sources are recoverable at git ref `16cfa5e`
(`packages/aethyme/src/eval/`, `packages/aethyme/evals/`,
`packages/aethyme-eval-ui/`). Frozen baseline reports remain at
`docs/reports/evals/`. The playground environment scripts
(`scripts/eval/`) remain live — they set up paired control /
tool-enhanced clones and are the natural testbed for future
multi-agent broker load scenarios.
