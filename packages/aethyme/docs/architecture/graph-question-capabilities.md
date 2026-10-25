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

Status reports `transport_healthy` for compatible, current committed fragment
integrity. With graph authority enabled, `healthy` additionally requires
`majority_has_observed_question`. Missing, empty, partial or legacy measurements
cannot produce a healthy content claim merely because their bytes validate.
The blocker `majority_question_prerequisites_not_observed` makes the gap explicit;
`action_required` is true. The existing human status exit becomes nonzero, while
JSON status keeps its observational zero exit and callers must inspect the fields.
Compatibility, uncommitted artifacts and materialization retain their earlier
next-action priority. Once those are current, the next action points to coverage
and question evidence rather than prescribing an ineffective refresh loop.

Authority-disabled repositories remain a cheap healthy no-op, with an empty
capability report and no content claim. Observed prerequisites remain a necessary
health condition, not proof that any impact query is complete: this change grants
no CI selection authority. Complete question-specific query qualification and
content semantics remain open under #213; the entire graph redesign is not done.

The `aethyme-graph-indexer` integration test `edge_production` exhaustively
classifies every schema `EdgeKind`. Seventeen kinds require positive witnesses
read back from persisted Playground fragments; nine are explicitly not produced
by the default indexers, with reasons tracked in #213: `Implements`, `Inherits`,
`Reads`, `Uses`, `Writes`, `Mocks`, `Configures`, `Decides` and `Deprecates`.
A new enum variant requires a compile-time classification; an emitted kind
missing its witness fails the test. A formerly unsupported kind observed in the
fixture also fails until its declaration is updated. The fixture includes the
existing documentation/test relationships and unchanged surface-flow inputs,
plus an explicit Python import and call. Its fragment counts must equal the
coverage projection. This is producer evidence, not proof of complete semantics
or coverage in every language. Existing language-specific and negative-resolution
tests remain necessary, as do question-specific query qualifications.


## Revision-bound documentation query

`aethyme graph impact --repo . --revision HEAD --diff changed-paths.txt
--mode documents --json` projects direct incoming `Documents` links into
`impact.documents`, alongside `direct` evidence and the existing coverage,
revision, provenance and limit fields. Human output also lists document paths.
The shared analysis envelope uses the versioned `graph-impact-documents` legacy
profile and the same `documents` array; other relationship arrays stay empty.

Seeds include every indexed node kind at each exact changed path, so file,
function and class references are considered without a prefix scan. Document
paths are deduplicated. The query bounds exact changed-path lookups as well as raw adjacency rows,
nodes and results. Missing paths consume the lookup allowance; reaching it leaves
the result incomplete without probing later paths. Missing subjects, malformed document endpoints or exhausted limits
make the result incomplete; stale or unavailable results expose no paths.
Documentation mode requires an explicit `documents` coverage entry, including
zero, but does not require test relationships. Revision-bound partial paths
are advisory only. No result narrows mandatory gates or proves unlinked prose
unaffected, and no nightly scheduler or documentation rewrite runs here.

The real CLI regression starts from a synthetic source and Markdown reference,
refreshes persisted fragments, materializes the graph, queries documentation
impact and checks stale-revision withholding. Separate store/provider controls
cover file/function/class targets, exact-path isolation, duplicate links,
reverse links, missing subjects, node/result/depth limits and coverage semantics.
These qualify a consumer of produced links; they do not claim every prose
reference or language is semantically resolved. CI sizing and session-bootstrap
qualification remain separate work under #213.

## Symbol-aware import query

The Imports impact lane seeds every indexed subject at each exact changed file
path, including functions and classes targeted by named Rust and Python imports.
When a typed importing File is reached, all subjects at that exact path are
seeded at the same import depth; containment does not consume an import hop.
This permits transitive named imports without treating Calls as Imports.
Expanded paths suppress cycles and duplicate importer results. Function subjects
also feed the existing explicit Tests traversal.

Changed-path lookups, indexed nodes, raw adjacency rows, depth and result counts
remain bounded. Missing subjects and absent, mistyped or unsafe importer endpoints
mark the observation incomplete. An exhausted allowance retains conservative
fallback; stale revision results expose no paths. The real CLI regression indexes,
links and materializes a synthetic Rust named-import chain before querying both
hops and checking stale withholding. Provider controls cover class/function
subjects, cycles, sibling isolation, explicit test links and incomplete budgets.
This is advisory evidence, not CI-selection authority or complete import semantics
for every language. Session-bootstrap qualification remains open under #213.
