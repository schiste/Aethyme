# Selected final Local integration checks

Last Updated: 2026-10-15

The [ledger](ledger.json) records actual sequential local commands on the aggregate
Local branch. Each run retains its complete original recorder metadata, raw log,
numeric status, exact source commit/tree, tracked working-tree status and diff
hash before and after execution. Inventory SHA-256 and byte counts cover every
copied payload. Copies preserve original output, including blank lines at EOF
that `git diff --check` may report; raw logs are not normalized.

## Recorded source and results

At clean source `b73452b4ce99f4f05dc026625c659d0d6f13bbce` (tree
`bf1629aa1b0c43129636212cf86c41af98ed5bdc`), all three commands exited 0 with
unchanged source: 21 analysis-view library tests, 38 collaboration-context
library tests and two public context-selection vectors. These are 61 passing
selected executions. They overlap earlier branch qualification and do not
replace independent adversarial review, final-head CI or held-out behavior.

## Remaining integration work

Native-job release and schema-23 qualification are recorded separately once
executed and reviewed. The exact T45 pressure/GC interleaving is also pending.
No later combined tree is credited by these earlier source-bound runs.
No model, provider trial or private held-out evaluator ran in this selection.
The six consumed FX17 provider errors remain unchanged; this ledger does not
establish brief usefulness or reliable autonomous operation.
