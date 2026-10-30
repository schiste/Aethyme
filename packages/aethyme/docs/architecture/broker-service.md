# Progressive broker service

Last Updated: 2026-10-30

The broker currently coordinates CLI processes through SQLite transactions and
file locks. A shared service will own recurring work and request scheduling,
while build, test and review workers remain separate processes. Every step must
preserve existing CLI contracts and remain usable after its predecessors merge.

## Merge sequence

1. **Protocol foundation (this change).** Add a bounded, versioned protocol and
   its tests. No process starts, no database changes, and no existing command is
   routed differently.
2. **Opt-in service.** Add explicit serve/status lifecycle commands and a private
   local socket with singleton ownership. Initially support health requests
   only. Existing operations continue to run directly.
3. **Shared cleanup jobs.** First add an inactive durable job store with IDs,
   deduplication, shared progress, deadlines and cancellation. Then connect
   explicitly requested GC planning to the service in a dependent PR. Applying
   cleanup needs a subsequent authorization contract and must reuse existing
   safety checks and journals; a service does not authorize deletion by itself.
4. **Recurring work.** Move automatic cleanup and monitoring behind service
   scheduling. Preserve existing coordinators while old clients coexist. One
   slow job must not prevent health or status requests.
5. **Request migration and default activation.** Move additional typed operations
   in bounded groups. Test mixed versions, disconnects, restarts and rollback
   before default routing changes. Do not retry a mutation locally after an
   ambiguous service failure: first reconcile its durable job record.

Dependent PRs may target their predecessor. Each PR must name its prerequisite,
document the behavior enabled at that point, and validate legacy operation with
the service absent. Do not remove the old path merely because the new endpoint
exists. Default activation is a separate reviewed change.

## Protocol v1: health only

The Rust contract lives in `aethyme_broker::service::protocol`. Its presence is
not a routing switch.

One connection carries one UTF-8 JSON request and response, each terminated by
one newline. A frame is at most 65,536 bytes including the newline. Writers check
the entire serialized value before emitting bytes. Readers reject incomplete,
oversized or malformed frames and unknown request fields/methods. Socket
deadlines must bound time independently of the byte limit.

Request:

```json
{"protocol_version":1,"request_id":42,"method":"ping"}
```

Success:

```json
{"protocol_version":1,"request_id":42,"result":{"Ok":{"pid":123,"version":"0.8.47"}}}
```

An incompatible request version receives a response with protocol version 1,
the original request ID, and
`result.Err = {"code":"unsupported_protocol","message":"..."}`. The message is
diagnostic prose, not a machine discriminator. Clients validate the response
version and request ID before consuming it. All listed fields are required;
there are no optional or null fields. Request IDs correlate replies only and
must never be used as durable idempotency keys.

The first transport must use a private directory owned by the current user,
restrict socket access to that user, fence concurrent startup, and refuse
unsafe path replacements. It must not expose shell execution or deserialize
arbitrary CLI argv. Repository identity, request context, authorization and
durable idempotency must be designed before introducing mutation methods.

## Opt-in foreground transport

After the transport PR merges, create a private directory and resolve its
canonical absolute path (for example with `pwd -P`), then run:

```sh
mkdir -m 700 /tmp/aethyme-service-example
aethyme broker advanced service serve /private/tmp/aethyme-service-example
```

From another terminal:

```sh
aethyme broker advanced service ping /private/tmp/aethyme-service-example --json
```

On Linux use `/tmp/aethyme-service-example` when that is its canonical path.
The directory must already exist, belong to the current user and deny group
and other access. Paths containing symlinks are refused. The socket is mode
0600; a permanent locked file in the directory provides singleton ownership.
Do not unlink that lock file while any service is running.

`serve` runs in the foreground until interrupted or stopped by its process
supervisor. No launch agent is installed and no existing broker request is
rerouted. `ping` emits the v1 Response JSON, with or without `--json`, and
returns a nonzero exit when the endpoint is absent, unsafe or incompatible.
Invalid arguments fail before opening a socket. Help, serve and ping do not
initialize, upgrade, reconcile or maintain repository state.

At most 16 client handlers run concurrently. Each frame has a two-second
absolute read deadline and socket writes have a two-second timeout. Overload
closes the additional connection without claiming execution. This is currently
safe because the only method is a read-only health request. A malformed client
is closed without a response; a parsed incompatible version gets the typed
error described above. A stale owned socket is recovered under the singleton
lock on restart. An unexpected file at the socket path is preserved and causes
startup to fail. Orderly library shutdown removes only the bound socket inode;
abrupt process exit leaves a socket for the next startup to recover.

The runtime directory is explicitly selected in this opt-in phase. Automatic
per-user discovery, startup, shutdown requests and migration of repository jobs
are later steps; this phase does not yet reduce existing broker process counts.

## Inactive durable job store

`service::jobs::JobStore` is a library foundation, with no service methods,
automatic workers, discovery or routing changes. Even `serve` and `ping` do
not open it. Its only typed operation describes GC **planning**, never apply,
arbitrary commands or CLI argument vectors.

The caller explicitly selects a trusted private SQLite file, separate from
`broker.db`, and must resolve the stable repository identity shared by linked
worktrees before submission. Opening storage grants no repository or cleanup
authority. Application ID `0x41534a31` and user version 1 distinguish this file
from broker or unrelated databases; incompatible identities and versions are
refused before schema or journal-mode changes. Transactions use a two-second
busy timeout, WAL and FULL synchronization. This PR introduces no broker schema
migration and never points a development job store at installed broker state.

Each accepted request receives a persistent positive integer job ID. A
repository-scoped idempotency key maps permanently to that job; after a lost
reply, `find_request` can recover the ID and outcome. Reusing a key with changed
options or a changed job deadline is refused. Different keys join identical
outstanding repository, typed operation options and absolute job deadline.
Different repositories, options or job deadlines remain separate. A client's
wait timeout is a separate future transport concern. After a terminal outcome,
a fresh key can request a fresh plan; old keys keep returning the old result.
Rows are retained without automatic pruning in this foundation.

Immediate SQLite transactions serialize submission and claim across independent
connections. A queued job may be claimed only once. The claim carries a random
128-bit token in addition to its job ID and worker generation; stale or
cross-database claims cannot update progress or completion. Progress is shared,
bounded and cannot move backwards. Completion persists either a JSON result
(at most one MiB), failure message or worker-acknowledged cancellation. Messages
are at most 4096 bytes. These internal records are not the socket v1 response
schema; the dependent worker adds chunked transport below. Retention remains
follow-up work.

The states are `queued`, `running`, `interrupted`, `succeeded`, `failed`,
`cancelled` and `expired`. Queued jobs whose deadline elapsed become expired
before claim. A running worker must enforce its deadline itself and report
what happened; the store never declares that a deadline stopped a live worker.
Cancelling queued work prevents dispatch. Cancelling running work only records
`cancellation_requested`; its state stays running until the worker reports an
actual outcome. Cancellation affects the shared job and all joiners. Repeating
cancellation on a terminal job leaves its recorded outcome unchanged.

After independently proving that a worker generation has lost ownership, a
controller may mark that generation's running jobs interrupted. Opening a
database or observing an elapsed deadline never supplies that proof. Interrupted
jobs retain their ID and progress, continue deduplicating identical requests,
and cannot be claimed again or completed through their old token. There is
deliberately no generic retry or reconciliation override yet. Connecting this
store to the singleton service must supply fencing, repository identity,
execution deadline enforcement and a bounded scheduling policy first.

## Acceptance before default activation

- Existing scripts, hooks and CLI JSON retain their behavior without a service.
- Concurrent cleanup callers share one job; distinct inputs remain distinct.
- Worker failure cannot wedge health/status or leak resource reservations.
- A client disconnect and a service restart preserve a discoverable outcome.
- Unknown outcomes are reconciled, never silently retried.
- Version mismatch fails clearly; rollback cannot produce two authorities.
- Playground load runs measure process count, latency and resource use.

## Opt-in GC planning worker

The dependent worker phase enables jobs only with:

```sh
aethyme broker advanced service serve /private/tmp/aethyme-service-example --jobs-for /absolute/repository
```

The repository must already have a valid broker database and verified repository
UUID. Startup uses a compatibility snapshot, without initializing the repository
or running normal startup maintenance. The service resolves the main repository,
Git common directory and database itself; clients cannot choose those paths or
supply commands. Linked worktrees resolve to the same configured repository.
Plain `serve` and v1 `ping` retain their health-only behavior and never open job
storage. Existing `gc plan`, cleanup continuations, monitoring and other broker
commands still run directly; default discovery and routing are not enabled.

One scheduler executes GC plans serially while independent bounded handlers serve
health and job requests. Jobs only inspect and plan: the worker calls the existing
planner with its cooperative deadline, including its observational size-cache
updates, and never calls GC apply or startup recovery. `full_scan` uses the job's
budget instead of the normal ten-second planning budget. Filesystem or SQLite
calls already in progress may outlive a cooperative deadline; partial plans retain
the planner's incomplete/deferred fields. No hard operating-system timeout is
claimed. Running cancellation records intent and is acknowledged only after the
planner returns. Cancelling affects every joiner. A planner error or unwind is
recorded as failure; it is never automatically retried.

For example, choose a unique request key and an absolute Unix-millisecond job
deadline within the next 30 minutes, and retain both exact values:

```sh
aethyme broker advanced service submit /private/tmp/aethyme-service-example --request-key my-plan-1 --deadline-ms <utc-ms> --json
aethyme broker advanced service lookup /private/tmp/aethyme-service-example my-plan-1 --json
aethyme broker advanced service status /private/tmp/aethyme-service-example <job-id> --json
aethyme broker advanced service result /private/tmp/aethyme-service-example <job-id> --json
aethyme broker advanced service cancel /private/tmp/aethyme-service-example <job-id> --json
```

Submit also accepts `--full-scan` and `--include-active-gate-cache`. Submit,
lookup, status and cancel emit the v2 Response envelope below with or without
`--json`; result assembles bounded chunks and emits the successful plan JSON
itself. A refused request or unavailable endpoint exits nonzero. Invalid syntax
fails before filesystem or socket work. A lost submission reply is ambiguous:
lookup the same request key or resubmit the exact original inputs. Never create
new work through a local fallback. A fresh key joins an identical outstanding
operation and deadline; expired historical keys still recover the original job.
At most 128 outstanding rows are admitted per service repository; retained
interrupted jobs count toward this bound. Completed history has no automatic
pruning in this phase.

### Protocol v2

V1 health remains unchanged. V2 uses the same 65,536-byte JSON-line framing,
absolute read deadlines and one request/response per connection. Every field
shown below is required, except the explicitly optional `repository_key`
request field described later. Unknown or duplicate fields and unknown methods
are refused.

```json
{"protocol_version":2,"request_id":42,"method":{"kind":"submit","idempotency_key":"my-plan-1","operation":{"kind":"gc_plan","include_active_gate_cache":false,"full_scan":false},"deadline_ms":1793376000000}}
```

Other method objects are `{"kind":"lookup","idempotency_key":"my-plan-1"}`,
`{"kind":"status","job_id":1}`, `{"kind":"cancel","job_id":1}` and
`{"kind":"result_chunk","job_id":1,"offset":0}`. Offsets are UTF-8 byte
positions in the serialized successful plan. Requests cannot apply a plan.

Every v2 response has `protocol_version: 2`, the original `request_id`, and
`result: {"Ok": <reply>}` or `result: {"Err": {"code": "...", "message": "..."}}`.
Reply objects have these exact shapes:

- `{"kind":"submitted","job":<status>,"joined":false}`.
- `{"kind":"lookup","job":<status-or-null>}`; null means no durable key match.
- `{"kind":"status","job":<status>}`.
- `{"kind":"cancel_requested","job":<status>}`; this is not a promise that
  running work has stopped.
- `{"kind":"result_chunk","job_id":1,"offset":0,"next_offset":8192,"complete":false,"data":"..."}`.

A status always contains `id`, `operation`, `state`, `deadline_ms`,
`created_at_ms`, `updated_at_ms`, `cancellation_requested`, `progress` and
`completion`. Timestamps are Unix milliseconds; operation and state use the
store contracts above. Progress is null until published, otherwise
`{"completed":0,"total":null,"message":"..."}`; an unknown total is null.
Completion is null until a terminal completion, otherwise
`{"kind":"succeeded"}`, `{"kind":"failed","message":"..."}` or
`{"kind":"cancelled"}`. Expired/interrupted rows have no completion. Status
never embeds a successful plan or repository paths. All fields are present,
including nulls; there are no omission-dependent defaults.

Each result chunk contains at most 8192 bytes of UTF-8 text before JSON escaping.
`next_offset` equals `offset + data.len()` in bytes; `complete` means end of the
serialized plan. An empty final chunk is valid at the exact end. Invalid offsets
are refused; a nonsuccessful job returns `not_ready`. Clients validate reply
version, correlation, job ID, contiguous offsets and total size (one MiB).

V2 error codes are `jobs_disabled`, `repository_mismatch`, `invalid_request`, `idempotency_conflict`,
`not_found`, `not_ready`, `unavailable` and `internal`; diagnostic messages are
not discriminators. A valid v2 method on plain health-only serve receives
`jobs_disabled`. Unsupported versions still receive the v1
`unsupported_protocol` envelope, and v2 clients refuse the version mismatch.
Malformed v2 method bodies with a readable header receive `invalid_request`;
malformed framing or headers close the connection without execution. Overload
or a lost response never proves whether a submission committed.

### Repository identity and scoped requests

The dependent identity phase adds `service repository <directory> [--json]`.
It sends `{"kind":"repository"}` and prints the v2 response envelope. A jobs
service replies with `{"kind":"repository","identity":{"key":"<64 lowercase hex characters>","repository_uuid":"<UUID>","root":"<canonical repository path>","common_dir":"<canonical Git common directory>"}}`.
All identity fields are required. A health-only service returns `jobs_disabled`.
This observes the owner captured and verified at service startup; it does not
initialize a repository or enqueue work. The opaque key excludes the worker
generation and remains stable across restarts of the same repository. Clients
must verify the returned owner against their independently selected repository
before trusting its key; learning a key alone is not owner verification.

A request may additionally carry a top-level `repository_key` string. Missing
means the legacy manually selected endpoint behavior; explicit null and
non-string values are invalid. The service compares a provided key with its
immutable repository identity before dispatching any method, including submit,
lookup, status, cancellation and result chunks. A mismatch returns
`repository_mismatch` without accepting or changing a job. Pin every request:
a handshake alone cannot exclude endpoint replacement between calls.

The library's `transport::scoped_job_request` requires the 64-character lowercase
key before connecting. Legacy `job_request` omits the new field entirely. Older
v2 servers reject the unknown field or method; scoped callers must surface that
refusal without downgrading to an unscoped request or executing locally. This
phase supplies the identity contract for subsequent caller migration; existing
GC commands retain their current direct path.

### Durable ownership, restart and rollback

Jobs are stored in a separate private `broker-service-jobs.sqlite` file under
the explicitly selected runtime directory. A private context file binds the
repository UUID and canonical paths, repository/common-directory filesystem
identities, job database inode, runtime-directory inode and permanent lock inode
to a random worker generation. Existing sidecars and context files must be
private, regular, owned, single-link files. Symlinks and replacements are refused
without deleting the unexpected data. The directory/lock/database identities
are rechecked for every store access. The lock remains held through worker and
client drain, including outstanding runtime handles.

After acquiring that exact retained lock and verifying context, restart marks
the previous generation's running jobs interrupted before accepting requests.
Interrupted jobs keep their original IDs and cannot be claimed again. Queued
jobs may run once; completed results remain readable. A replaced lock or directory
cannot authorize fencing of the old generation. A crash before initial context
publication leaves a database without context; startup preserves it and refuses
until the operator reviews that incomplete bootstrap. This deliberately trades
automatic bootstrap repair for preserving uncertain ownership evidence.

Use the same runtime directory for all clients that should share work. Separate
explicit directories are independent service instances in this opt-in phase;
there is no global singleton or automatic endpoint discovery yet. Rolling back
to health-only serve does not claim, reconcile or delete jobs. Existing direct
commands remain usable, but operators must reconcile interrupted work before
issuing an equivalent fresh plan. Default activation and cleanup application
remain separate changes.
