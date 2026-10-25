# Materialized documentation sections

Last Updated: 2026-10-25

The fragment indexer emits `Documents` links from heading-bounded `DocSection`
nodes to explicitly resolved code subjects. Materialization previously copied
those edges but mapped only the documentation-file overlay. Their section
endpoints therefore did not identify any persisted typed node, so document
queries could silently return no linked documentation.

Actual fragment owner IDs are joined through their exact indexed paths, so
relocation into a differently named materialization checkout does not sever
ownership when model overlay IDs use the new checkout name.

The runtime map now gives every section with a recognized documentation-file
owner its own `DocNode`. Its ID combines the parent document ID with the stable
schema section hash, its title is the heading, its path is the parent file path,
and its `doc_type` is `section`. This preserves distinct sections with identical
headings and maps their relationship endpoints before persisting edges. Unknown
or invalid owners are not guessed or adopted. File-level document nodes remain.
The generic docs relation can return multiple sections at one path; callers
asking for affected files should deduplicate paths.

Derived redb schema 11 invalidates stores lacking these endpoints. It changes
no broker database or committed fragment schema. Rematerialize the disposable
store from verified committed fragments with `aethyme graph materialize --repo .`;
no source indexing is required for that repair. The materialization-cache identity
includes the derived schema version, so an older cached artifact cannot satisfy
the new endpoint contract.

The synthetic regression reads real indexed fragments, preserves duplicate
headings and a plain-prose negative case, checks relocated-checkout ownership and deterministic map IDs, atomically
materializes the store and retrieves both linked sections through the existing
`docs_for` API. This repairs the producer-to-store bridge; it does not claim all
prose references or language semantics are resolved.
