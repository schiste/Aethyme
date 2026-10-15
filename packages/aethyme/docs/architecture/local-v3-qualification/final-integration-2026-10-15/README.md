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

At clean source `e105edc81a4cdf9464e7bbfdca54540cea2f7bfb` (tree
`5cef88b29c2a669c0669edef0f2dbde1c2d6b714`), six offline FX17 model-protocol
and disposable campaign-marker controls also passed, with unchanged source. These
use mocks: zero model requests, private evaluator runs or changes to the consumed
live campaign marker. An earlier recorder preflight rejected an unsupported
environment argument before launching tests; its separate failure record is
retained without test credit. These executions do not qualify the failed live
FX17 campaign.

## Retained CI failures

The ledger also retains the complete workspace logs for PR #783 at `99bbc0b1`
and aggregate PR #777 at `f47ad6ea`. Each CI merge-ref checkout ran 4,566 tests:
4,565 passed, one failed and 32 were skipped. Both exited 100 because the
discarded-result ratchet found 144 sites against the unchanged baseline of 143.
The records name the actual CI merge commit and tree, separate from the PR head.
These failed commands receive no passing-command or ignored native-profile
credit. Their complete original logs are retained.

## Initial full workspace run and native fixture repair

At clean aggregate `86c982eeecd92e473e309bf0f13dfdde1379e6ac`, the official
`test-like-ci.sh --full` command completed with exit 100: 4,569 tests ran,
4,489 passed, 80 failed, and 38 were skipped. Source stayed unchanged. The complete
raw log and JUnit report are preserved. The original driver probed the shared
Cargo target for JUnit; a separate reconciliation record binds the fresh actual
workspace report without rewriting that original batch record. The remaining
four planned stages were unrun after the full-suite failure.

Five valid native-dispatch fixtures inherited the official runner's `GIT_*`
configuration. A test-only child wrapper gives those fixtures private HOME,
XDG and temporary roots and removes inherited Git variables only in the child;
the production environment refusal remains strict. Sixteen selected native and
controller tests passed under the same injected configuration. Removing the
wrapper's environment cleanup caused the exact child assertion and parent test
to fail (exit 101); the unique backup was restored byte-for-byte, and the restored
exact test passed. Their metadata records the precise working-tree diff at
source `86c982`; these are 17 overlapping positive executions and one negative
control, not a successful full workspace run.

Several other first-run failures involved older shell fixtures and temporary
paths containing spaces. A subsequent full retry uses a whitespace-free,
off-Git temporary root; the official runner, Git isolation, assertions, limits
and failure behavior remain intact. This does not qualify arbitrary paths with
spaces. The retry and the remaining four checks are pending.

## Remaining integration work

The separately reviewed [native-job release and schema-23 qualification](../native-job-lifecycle-2026-10-15/README.md)
retains its successful checks, deliberate guard failures and failed or incomplete
attempts. The separately [reviewed T45 interleaving](../t45-pressure-gc-interleaving-2026-10-15/README.md)
records four bounded pressure cases in two passing native invocations and five
intended guard failures, with their source and environmental limits.
No later combined tree is credited by these earlier source-bound runs.
No model, provider trial or private held-out evaluator ran in this selection.
The six consumed FX17 provider errors remain unchanged; this ledger does not
establish brief usefulness or reliable autonomous operation.
