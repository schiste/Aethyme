# Persisted code-file endpoint materialization

Last Updated: 2026-10-25

Committed fragments retain the repository identity used when indexing. Runtime
structure overlays derive identities from the current materialization checkout.
A disposable checkout or directory rename can therefore produce different IDs
for the same exact file path.

The runtime adapter maps each actual fragment `File` ID to the known runtime
File through its exact indexed path. It requires the node path to match its
owning fragment path. This translation runs before relationship projection, so
an Imports source remains queryable as a typed File when its target resolves to
a Function. NonCodeFile owners keep the separate documentation and configuration
projections; no ownership is inferred from similar names or path prefixes.

Derived graph schema 12 invalidates stores written before this endpoint binding.
It follows schema 11's documentation-section projection. The broker database
schema and committed fragment format are unchanged. Explicit graph materialization
reconstructs the derived store from verified fragments; queries do not rewrite
fragments or silently rebuild stores. The immutable materialization cache includes
the graph schema version, preventing reuse of the older projection.

The regression starts with real Rust sources containing a cross-file named
import, indexes and links their fragments, moves the checkout, rebuilds the map
without reindexing, then materializes the store. It checks the actual import
source's File identity, the Function destination, deterministic repeated builds,
and a typed source lookup through persisted incoming adjacency. This is endpoint
qualification only: import impact traversal must separately seed symbol targets
and retain conservative coverage and limit handling before claiming CI sizing.
