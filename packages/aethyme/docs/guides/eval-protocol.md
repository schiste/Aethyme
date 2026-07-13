# Eval Protocol (removed)

Last Updated: 2026-07-13

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

Sources are recoverable at git ref `16cfa5e`
(`packages/aethyme/src/eval/`, `packages/aethyme/evals/`,
`packages/aethyme-eval-ui/`). Frozen baseline reports remain at
`docs/reports/evals/`. The playground environment scripts
(`scripts/eval/`) remain live — they set up paired control /
tool-enhanced clones and are the natural testbed for future
multi-agent broker load scenarios.
