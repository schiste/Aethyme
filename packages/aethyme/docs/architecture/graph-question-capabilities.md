# Question-oriented graph observations

Last Updated: 2026-10-25

`aethyme graph status --json` includes `question_capabilities` schema 1. It
projects existing committed coverage into three questions for each measured
language, without indexing source, querying the derived graph or changing gates:

| Question | Observed prerequisites |
| --- | --- |
| CI sizing | Import relationships and test-to-target links |
| Documentation impact | Document-to-subject links |
| Session navigation | Files and symbols |

Calls can refine CI precision but are not required for the imports lane. An
unresolved import prevents CI prerequisites from being fully observed. Counts
are observations from the producer, not evidence that every relationship was
recognized or resolved, nor that a particular query has completed.

Each question carries a `state` and stable `reasons`:

- `unknown`: coverage or the language's use measurement is unavailable. Legacy
  coverage without the new evidence map remains unknown, not zero.
- `not_observed`: a current measurement emitted none of the required signals.
  This describes that measurement; it does not prove no semantic impact.
- `partial`: some prerequisites exist, but another is missing, coverage has a
  known gap, revision/tree binding is incomplete, language file attribution
  differs, or imports remain unresolved.
- `observed`: all listed prerequisites were emitted, coverage is safe and
  revision/tree-bound, and the language inventory agrees. A complete bounded
  query and its consumer policy are still required before relying on a result.

`grants_ci_selection_authority` is always false. Incomplete, stale or truncated
impact results must retain the existing conservative fallback; this projection
cannot narrow configured gates, skip tests or turn an empty query into proof.

`majority_languages` is a descending file-count prefix covering strictly more
than half of the observed code files. It includes all ties at the boundary and
unsupported code files; non-code files are excluded. The explicit
`majority_scope: observed_code_files_only` prevents this from claiming coverage
of excluded or unindexed files. `majority_has_observed_question` is true only
when every selected language has at least one question with observed
prerequisites. An empty inventory is false.

The existing `healthy` and lifecycle exit behavior still describe graph
transport/integrity. Authority-disabled repositories remain a cheap healthy
no-op, with an empty capability report and no content claim. This instrument is
one part of #213; question-specific query qualification, producer guardrails
and integration of content readiness into lifecycle health remain open. It is
not evidence that the entire graph redesign is complete.
