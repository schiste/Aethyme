# Progressive broker service

Last Updated: 2026-10-29

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
3. **Shared cleanup jobs.** Add durable job IDs, deduplication by repository and
   operation inputs, shared progress, deadlines and cancellation. Make explicit
   cleanup requests available through the service. Reuse existing cleanup safety
   checks and journals; a service does not authorize deletion by itself.
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

## Acceptance before default activation

- Existing scripts, hooks and CLI JSON retain their behavior without a service.
- Concurrent cleanup callers share one job; distinct inputs remain distinct.
- Worker failure cannot wedge health/status or leak resource reservations.
- A client disconnect and a service restart preserve a discoverable outcome.
- Unknown outcomes are reconciled, never silently retried.
- Version mismatch fails clearly; rollback cannot produce two authorities.
- Playground load runs measure process count, latency and resource use.
