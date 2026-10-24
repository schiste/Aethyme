# Early task overlap warnings

Last Updated: 2026-10-24

Broker start/adopt/reuse already compare branch and PR identities. They also
report `task_mentions_path` when descriptions explicitly name a common relative
file path containing a directory, and `same_task` for identical case/punctuation-normalized descriptions of
at least four words. These local observations work without graph data or a
provider request. Only the first 8,192 task characters and first 64 eligible
paths are considered; truncated descriptions never claim exact task equality. URL, absolute and traversal paths are excluded.

Warnings name the other session and its task. They are advisory: lexical
similarity does not establish a base failure, assign ownership, or authorize
stopping another agent. Coordinate with the named owner before duplicating a
repair. Different descriptions can still refer to the same work; declaring
planned paths and reviewing current session activity remains necessary.

This is a bounded fallback for #760. It does not implement a default-branch
failure incident registry or turn normal cooperative leases into exclusive
edit claims. Those requirements remain open.
