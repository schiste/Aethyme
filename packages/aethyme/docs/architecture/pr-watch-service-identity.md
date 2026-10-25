# PR-watch service generation and runtime identity

Last Updated: 2026-10-25

The `aethyme.pr-watch-service-plan/v1` JSON adds `generation`. It binds repository
and user identity, paths, executable and helper hashes, interval and the rendered
launchd unit. Generation is derived without a self-reference:

1. Construct the plan with empty `generation` and `digest` and render its unit
   with an empty generation environment value.
2. SHA-256 the serialized plan in struct field order to obtain `generation`.
3. Render the unit with that generation and hash the complete serialized plan
   with only `digest` empty to obtain the operator confirmation digest.

The generated unit invokes Python with `-I -B`: isolated imports, no user site
packages or PYTHONPATH influence, and no bytecode caches in the helper directory.
It explicitly sets the reviewed HOME so runtime configuration and adapter lookup
use the planned user namespace. It sets the health opt-in, generation and expected canonical broker
executable SHA-256. Planning still writes nothing and never activates a service.

The native planner also reads each executable through one no-follow,
nonblocking descriptor, checking executable regular-file type before hashing.
It retains the 256 MiB cap and compares descriptor and named-path identity,
length, mode and change/modification times before accepting the hash. Replacement
or mutation refuses planning; this is conservative race detection rather than
an atomic guarantee for a later exec. The byte cap cannot interrupt an individual
filesystem operation that stalls.

The monitor requires `AETHYME_PR_MONITOR_GENERATION` and
`AETHYME_PR_MONITOR_BROKER_SHA256` together, as 64 lowercase hexadecimal digits.
Empty or absent values preserve the existing manual monitor defaults. A partial
or malformed configuration refuses before any broker command or provider work.
Before dispatch it verifies the absolute broker path through a no-follow,
nonblocking open, requires a regular executable, and hashes at most 256 MiB.
It checks the descriptor and named path identity, size and modification/change
times before accepting the digest. Hashing shares the existing 90-second I/O
budget with tick, listing and delivery; an expired budget refuses before open.

The metadata-only `aethyme.pr-monitor-health/v1` record adds optional fields:

| Field | Meaning |
| --- | --- |
| `generation` | Valid configured service generation, or null for an unbound/manual pass |
| `identity` | `not_configured`, `verified`, or `failed` for the pre-dispatch executable check |

An existing record without these fields is unbound. A matching generation and
`verified` identity do not prove successful polling, notification delivery or
freshness. Consumers must also check the finished time, clock skew, and each
pass status. A failed check preserves the configured valid generation for
correlation, records only the failure class, and starts no broker command.
No transcript, arguments, environment dump or provider output is stored.

These checks do not make filesystem reads or path replacement atomic with exec.
Use a versioned immutable runtime path; do not replace that version's files in
place. A retired or changed runtime requires an explicit reviewed service
update. The Python launcher identity is pinned in the native plan but is not
rehashed by the already running Python process. The router remains responsible
for its paired engine validation. Scheduler lifecycle commands, service update
reconciliation and live transport qualification remain open for #887.
