# Selected final Local integration checks

Last Updated: 2026-10-15

The [ledger](ledger.json) records actual sequential local commands on the aggregate
Local branch. Each run retains its complete original recorder metadata, raw log,
numeric status, exact source commit/tree, tracked working-tree status and diff
hash before and after execution. Inventory SHA-256 and byte counts cover every
copied payload. Copies preserve original output, including blank lines at EOF
that `git diff --check` may report; raw logs are not normalized.
Byte-exact recorder metadata, logs, JUnit reports and retained runner scripts
include host-specific absolute paths, including the local account name, worktree,
shared target and temporary directories.

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

The later sensitive-content draft #785 at `65f0bce9` ran 4,575 tests in its CI
merge-ref checkout: 4,566 passed, nine failed and 32 were skipped (exit 100).
Eight failures came from historical-schema test fixtures retaining the new
schema-24 tables, and one from the changed command help snapshot. The fresh CLI
draft #784 at `58c47074` failed compilation in its workspace and lint jobs
(exit 101); neither job supplies an executed-test result. The complete logs and
actual checkout commit/tree/parents are preserved. These remain failed attempts
even after subsequent source repairs.

The subsequent #785 CI checkout at `cc31a0de` ran 4,576 tests: 4,573 passed,
three failed and 32 were skipped (exit 100). The synthetic-content composite
failed its retrieved-record token assertion; the native-job test expected
schema 23 but observed 24; and the help snapshot was stale. Its exact CI
checkout/tree, complete raw log and failure metadata are retained. This was
the earlier composite, and supplies no evidence for the later frozen v4
view-cleanup protocol or its controls.

At #784 head `3f14f83d`, all four OSS jobs failed compilation (exit 101):
workspace tests, strict lint, examples/documentation, and the native product
path. Two updated callers lacked typed error conversions for wave loading and
archive I/O. Their complete logs and actual merge checkout `ef470cc8` are
retained. These jobs executed no qualifying tests and receive no passing credit.

At #785 head `69e8202a`, strict lint failed with exit 101 for three unused
test helpers and a non-octal permissions literal. The workspace job was cancelled
before completion; its raw log retains fifteen distinct first-attempt failures
reporting `UnknownWork` from fixtures that had left their wave verifying. No
completed-suite denominator or passing-command credit is assigned. At later head
`8387fd9f`, strict lint failed with exit 101 for one needless borrow in a new test
helper. Its workspace command was cancelled with eight distinct first-attempt
failures: six view-residual assertions, terminal view cleanup and the synthetic
composite. No completed-suite denominator is assigned. At head `94282330`,
strict lint failed for a needless borrow in a generation-directory test
(exit 101). That head’s completed workspace command ran 4,601 tests:
4,600 passed, one failed and 32 were skipped (exit 100). Its sole failure was
the changed `collab` help snapshot. These complete logs and exact checkout
metadata remain preserved.

The reviewed cleanup/CLI merge at #784 head `0ec6d692` and aggregate #777
head `f1abe252` both failed strict lint (exit 101): the old composition wrapper
became unused in production, the capture retention row needed a named type,
and the final retainer use had an unnecessary dereference. Complete logs and
primary checkout metadata bind these failures to merge commits `c71540b1` and
`ab07afac`, respectively. No test result or passing-command credit is assigned
to these lint failures; their other jobs are reported separately.

The #777 `f1abe252` workspace job was cancelled after 65 distinct first-attempt
failures; no numeric command exit or completed-suite denominator is assigned.
The #784 `0ec6d692` workspace job completed with exit 100: 4,623 tests ran,
4,554 passed, 69 failed and 32 were skipped. Complete raw logs, first-attempt
failure lists, retry observations, API snapshots and actual merge-ref checkout
metadata are preserved byte-for-byte. These failed attempts do not qualify
later fixture repairs or supply passing-command credit.

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
spaces.

## Clean-source workspace retry

At clean source `1d0ce2647f277a7246d0994d4f935f1ca1bc183d` (tree
`8bdefa81de7f2b628eeed0ee505e1294fd3fb762`), all five sequential checks exited
0 with unchanged, clean source. The official full suite ran 4,569 tests: all
4,569 passed, 38 were skipped. Formatting and strict workspace/all-target Clippy
passed. Workspace examples passed seven tests. The documentation command passed
with zero documentation cases. The complete original logs, numeric statuses,
fresh JUnit report, driver, recorder, environment and batch record are retained.
These workspace checks overlap selected earlier executions and do not execute
the separately ignored native or once-final model profiles.

This source predates the Local sensitive-content response and fresh command-line
integration work. It supplies no passing evidence for those later changes.

## Source-bound native results and remaining integration work

The separately reviewed [native-job release and schema-23 qualification](../native-job-lifecycle-2026-10-15/README.md)
retains its successful checks, deliberate guard failures and failed or incomplete
attempts. The separately [reviewed T45 interleaving](../t45-pressure-gc-interleaving-2026-10-15/README.md)
records four bounded pressure cases in two passing native invocations and five
intended guard failures, with their source and environmental limits.
The Local T56/D29 sensitive-content response and fresh independent reviewer rehearsal
remain open. Review found that the existing native fixture constructs its two
contributors serially and supplies wave availability and verifier policy through
library APIs. It does not qualify fresh independent contributor processes through
the command line. No later combined tree is credited by these earlier
source-bound runs.
No model, provider trial or private held-out evaluator ran in this selection.
The six consumed FX17 provider errors remain unchanged; this ledger does not
establish brief usefulness or reliable autonomous operation.

## Later checkout, formatting and artifact-index failures

The preserved CI bundle for #784 head `3f5e23ad` and #785 head `5e7fe7bf`
binds the actual Actions merge checkouts, complete trees, source blobs, API
responses and full raw logs. Both Windows jobs failed checkout with exit 128
because nested frozen evidence paths exceeded Git for Windows path limits;
no build or native smoke ran. The #784 formatting check failed before Clippy.
The #785 workspace command completed 4,602 tests: 4,601 passed, one failed
and 32 were skipped (exit 100), with missing dates in three frozen Markdown
payloads. The #784 workspace job was cancelled after 50 distinct first-attempt
failures; observed progress 2,274/4,623 is not a completed-suite denominator.
Its numeric command exit and completed counts remain unknown. Early log API
404 and ANSI-output refusals are preserved alongside the later successful GET.

The CI payloads use short opaque `.raw` storage names with original logical
paths, media types, byte counts and hashes in `ledger.json`. Frozen source and
Markdown bytes are preserved as evidence, rather than edited as maintained
documentation. The T56 package separately uses the same portable representation.

The original artifact verifier at `9795ec1b` also failed: its 424 errors came
from resolving 212 valid T56 payload paths against the wrong base. Its complete
output and script are retained. An independently reviewed exact-schema adapter
uses that package's documented `payloads/` root; at the same source it checked
753 unique payloads and 37 links with zero errors. The later portable package
adds the byte-exact original ledger as one further artifact. These checks
verify retained evidence and do not qualify runtime behavior.
