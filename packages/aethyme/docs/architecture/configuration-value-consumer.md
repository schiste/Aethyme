# Configuration value consumer

Last Updated: 2026-10-25

The engine retains configuration-value facts already emitted by the graph
indexer for conservative TOML, JSON and YAML inputs. `ConfigNode.value` is
`null` for a file-level configuration node. For an addressable key it contains
`config_path`, `value_hash`, `start_line` and `end_line` (inclusive, one-based).
The projection stores a hash rather than the raw scalar value. Paths and symbol
references are navigation metadata, not a secret-redaction guarantee.

`graph-configs --target <symbol>` includes incoming `References` only when the
source is a typed configuration-value node, alongside existing `Configures`
and `EntrypointFor` relationships. Qualified symbol references are explicit
indexer facts; bare strings are not linked. This does not prove that runtime
code reads the key and grants no CI-selection or execution authority.

Fragment owner IDs are rebound through the exact indexed file path when a
checkout is relocated. Key identities, source locations and references remain
available through both in-memory navigation and the materialized read-only
store. File overviews and dependency extraction continue to count configuration
files rather than every addressable key.

The derived graph-store schema is 13. Stores at schema 12 or earlier must be
rebuilt; they are refused rather than decoded using the new ConfigNode layout.
The derived cache identity includes this version. Committed fragment formats
and broker database schemas do not change.

This supplies configuration navigation for issue #213. Calls precision and
complete question-level qualification remain separate outstanding work.
