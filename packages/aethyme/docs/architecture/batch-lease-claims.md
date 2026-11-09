# Batch lease claims

`aethyme broker advanced leases claim <path> [<path>...] --session <id>
[--ttl <seconds>] [--json]` accepts multiple paths in one invocation. Single-path
claims retain their existing output contract.

Multiple claims are independent and are not atomic. The response is a JSON
object with `session_id`, `atomic: false`, `failures`, and a `claims` array in
request order. Each element identifies `requested_path` and either contains
the usual claim `report` and a `scope_warning`, or an `error`. A nonzero exit
means at least one claim failed; successful claims are retained, including
claims attempted after a failed path. Review every result before editing.
