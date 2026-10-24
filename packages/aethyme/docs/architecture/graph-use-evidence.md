# Graph use evidence by language

Last Updated: 2026-10-24

The coverage artifact embedded by `graph status` includes optional
`use_evidence_by_language`. A current indexer measures this from the same
fragments and file observations as its index pass, without source rereads or
another parse. Existing artifacts without the field, and unavailable coverage,
report null: not measured. A present empty map means no languages observed.
A zero counter means no matching evidence emitted in that measured pass.

Each language has file and code-symbol counts for bootstrap/navigation,
resolved imports and calls for impact inputs, and tests and documents for
coverage/navigation inputs. Imports and calls are attributed to the source
language. Tests and documents describe the destination subject, so Markdown
that documents Python adds Python document evidence. Edges lacking a concrete
destination are counted as unresolved imports/calls separately; missing
endpoint language is explicitly `unknown`. Excluded files are outside these
observed-language counts and remain visible in the existing coverage gaps.

These counts are observations, not an assertion that any question is fully
answerable. One call edge does not prove complete impact analysis. A test edge
does not prove a complete test suite. No edges may mean missing support or a
source with no such relationship. Consult parser coverage, gaps, revision/tree
binding and the exact relationships before using them. Existing safe-to-use
checks remain in force, and this field grants no gate-narrowing authority.
It is an additive optional field in graph coverage schema 1; old artifacts
remain readable without being silently reported as measured zero.

This addresses the evidence visibility part of #213. Multi-language question
qualification and any test-selection policy require separate evidence and
remain open. Native regression execution is pending CI; no local native build
was performed for this slice.
