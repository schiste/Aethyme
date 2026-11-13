# Concurrent Host Resource Coordination

Last Updated: 2026-10-01

Aethyme can run validation concurrently from independent clones without
sharing fixed ports, Docker project names, database names, or host-capacity
slots. A repository may declare gates in `.aethyme/gates.toml`, or keep an
existing runner and place only its process lifecycle under `resources run`.
In both cases the repository defines what validation means; the broker owns
host allocation and supervision, not gate selection.

No daemon or central runner is required. A per-user SQLite registry coordinates
all Aethyme processes on the host. Its default location follows the platform's
user state convention and can be overridden with `AETHYME_HOST_STATE_DIR` for
tests or an explicitly isolated environment.

The entire `broker advanced resources` surface is host infrastructure. It does not
discover a Git repository, require Aethyme deployment, or enforce repository
schema compatibility. It is safe to invoke from an unenrolled checkout or a
private runtime directory. Repository and worktree fields in a request are
opaque coordination/audit identities, never enrollment authority.

## Supervise An Existing Runner

Use the broker-owned lifecycle without migrating an established validation
pipeline:

```bash
aethyme broker advanced resources run request.json --wait 30m \
  --cleanup-command './scripts/cleanup-exact-owned-services' \
  -- ./scripts/existing-pre-push
```

The command acquires the complete bundle, exposes allocation variables plus
lease id and generation, renews at one third of the request TTL, forwards
`INT`, `TERM`, and `HUP` to the child's process group, and preserves a failing
child exit code. The ownership token never enters the child environment.

If supplied, `--cleanup-command` runs through `sh -c` with the same public
allocation environment after the original child process group is observed
absent. Release requires the cleanup command to succeed and its process group
to be observed absent too. A failed or unspawnable cleanup, a surviving or
unobservable process group, or lost renewal authority leaves the exact
generation quarantined. If the child succeeded but lifecycle cleanup did not,
the command exits 70; otherwise a failing child's original exit code wins.

Before calling environment preparation or spawning the child, `resources run`
commits a supervised capacity hold with the lease. This protects capacity if
the supervisor exits before it can prove its owned process groups are absent.
The hold survives dead-holder reaping and ordinary `release`; only a verified
clean terminal path or an operator's exact-generation `reconcile` clears it.
Treat a supervised `request_id` as one-shot: use a new ID for every run, since
reusing it never starts another child. The bounded check sees only the original
process groups; a descendant that deliberately leaves its group or session is
outside this proof.

The request and report JSON schema remains version 1. The host-resource SQLite
store has its own version and writable open atomically migrates a version-1
store to version 2. Current read-only clients can still read version-1
stores; older binaries fail closed when opening a version-2 store. Already-open
version-1 connections are fenced by database triggers from releasing or
reaping a held lease and from inserting capacity that would exceed the held
pool's limit. Existing headroom in that pool remains usable.

Normal child stdout and stderr remain untouched. With `--json`, bounded wait,
grant, signal, cleanup, and final lifecycle records are emitted as JSON lines
on stderr, so automation can observe coordination without corrupting the
existing runner's stdout protocol.

## Sandboxed execution

Coordination state is host-scoped, under `~/Library/Application Support/Aethyme`
by default, because it coordinates across worktrees and repositories on one
machine. A sandbox that confines writes to a single checkout therefore cannot
open it, and the failure is a permission denial rather than a missing install.

Sandboxed contexts are supported, and the choice is between two different
things:

- **Participating.** Grant the process access to the host state path. It then
  coordinates with every other session on the machine: leases, sessions and host
  resources are shared, which is what makes contention visible and refusals
  correct. This is the right answer whenever anything else on the host also runs
  Aethyme.
- **Isolating.** Point `AETHYME_HOST_STATE_DIR` at a writable location. This
  always succeeds, and gives the process a **private coordination domain**. It
  will not see leases, sessions or host resources held by anything else, and
  nothing else will see its. Two sandboxes with separate state directories do
  not coordinate with each other at all.

Isolation is correct for a disposable build, a test harness, or CI where the
checkout is the whole world. It is wrong wherever concurrent sessions must not
collide, because each will believe it holds resources the others also believe
they hold.

There is no third option in which a confined process participates in host-wide
coordination without access to host-wide state: the shared state *is* the
coordination.

## Declare A Gate Bundle

Resources under one gate are acquired atomically: either every value is
granted or the command does not start.

```toml
[[gate]]
name = "integration"
command = "./scripts/test-integration"
cost = 3
cache = false
resource_ttl_seconds = 300
resource_wait_seconds = 120

[[gate.resources]]
key = "docker_project"
kind = "namespace"
prefix = "my-project"

[[gate.resources]]
key = "postgres_port"
kind = "tcp_port"
start = 55000
end = 55999

[[gate.resources]]
key = "database"
kind = "namespace"
prefix = "test-db"

[[gate.resources]]
key = "heavy_slot"
kind = "capacity"
pool = "integration-tests"
units = 1
limit = 2
```

The four generic resource kinds are:

- `namespace`: produces a stable, collision-resistant name for this run;
- `tcp_port`: chooses one broker-free, locally bindable port in an inclusive
  range;
- `capacity`: consumes units from a named host-wide pool;
- `exclusive_key`: permits one active owner of an exact named resource.

Resource keys become normalized environment variables. The example above
receives `AETHYME_RESOURCE_DOCKER_PROJECT`,
`AETHYME_RESOURCE_POSTGRES_PORT`, `AETHYME_RESOURCE_DATABASE`, and
`AETHYME_RESOURCE_HEAVY_SLOT`. Every gate with resources also receives
`AETHYME_RESOURCE_LEASE_ID` and `AETHYME_RESOURCE_GENERATION`. The ownership
token used to renew and release the bundle is never exposed to the child.

Allocation values are recorded in lease inventory and gate provenance. Do not
put database passwords, connection URLs containing credentials, or other
secrets in resource keys, prefixes, or allocation values. The generic lease
does not provide a secret-delivery mechanism or provision a PostgreSQL role,
database, or schema.

`AETHYME_TEST_DB_SUFFIX` identifies the session and gate, so retries of the
same gate can reuse it. It is not a unique run identifier. For a per-run name,
declare a `namespace` resource and use its allocated value; validate or quote
that value for the database's identifier rules. A unique name alone does not
establish database access isolation or clean up prior runs.

A broker-chosen port is broker-free and OS-bindable at the moment it is
granted, but the generic contract cannot hold the socket open while an
arbitrary application binds it. Between the grant and the bind, a
non-Aethyme process can still win that race, and the application will see
`EADDRINUSE`. That is a real limit of this contract, not a misconfiguration,
so applications should fail clearly and retry rather than assume exclusivity:

```sh
# Bind an ephemeral port and report it, so the caller reads the real value
# rather than assuming the one it asked for.
port="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')"
export TEST_DATABASE_URL="postgres://localhost:$port/$AETHYME_RESOURCE_DATABASE"
```

Socket activation — passing a bound descriptor to the child — would close the
race properly, but it needs an application-specific protocol and is outside
this generic contract.

Use these values in repository scripts rather than fixed names:

```sh
created_container_ids=""
cleanup() {
  for container_id in $created_container_ids; do
    docker rm -f "$container_id"
  done
}
trap cleanup EXIT INT TERM

docker compose --project-name "$AETHYME_RESOURCE_DOCKER_PROJECT" up -d
created_container_ids="$(docker compose --project-name "$AETHYME_RESOURCE_DOCKER_PROJECT" ps -q)"
export TEST_DATABASE_URL="postgres://localhost:$AETHYME_RESOURCE_POSTGRES_PORT/$AETHYME_RESOURCE_DATABASE"
./scripts/run-integration-tests
```

The broker renews the bundle during a long gate and releases it after success,
test failure, or spawn failure. If renewal authority is about to expire, it
terminates the gate's process group rather than let an unowned process keep
using a resource. A crashed owner expires into quarantine; it is not silently
reallocated until the operator confirms exact cleanup with the recorded lease
generation.

`resource_wait_seconds` controls ordinary contention before the command
starts. Its default is `0`, preserving fail-fast behavior. A positive value
retries only an unavailable bundle, reports bounded waiting progress, and
returns the original `resource_contention` diagnosis at the deadline. Invalid
requests, storage errors, and ownership failures are never retried.

## Prepare Dependencies For Isolated Worktrees

A worktree is a bare checkout. It has no `node_modules/`, no `.venv/`, and no
package-manager store, so a gate that shells out to `pnpm test` or `pytest`
fails on a fresh worktree in a way that has nothing to do with the change under
review. `.aethyme/prepare.toml` declares that install step once, and the
broker runs it per session.

`aethyme init` drafts the file from the repository's manifests. The draft is
only emitted where an install command **deterministically creates every
declared output**, because `prepare` records `failed` for an output that never
appears and a permanently red status is worse than a missing declaration. That
admits Node (`node_modules/`) and uv (`.venv/`), and deliberately excludes:

- **Cargo** and **Go**, which populate host-global stores and write nothing
  into the worktree. A bare Rust or Go checkout already satisfies its gates, so
  a step would declare an output its own command never creates. The draft says
  so in a comment instead.
- **Poetry**, which places the environment outside the worktree unless
  `virtualenvs.in-project` is set, and **plain pip**, which installs into the
  interpreter and therefore leaves nothing to check.

```bash
aethyme broker submit prepare status --session <id>   # read-only, no network
aethyme broker submit prepare --session <id>          # the only execution boundary
```

Each step declares `inputs` (hashed into the preparation digest) and
`outputs` (checked after the command runs). `cache` chooses the store:

- `worktree_local` — each worktree installs its own copy. Correct default.
- `repository_shared` — exports `AETHYME_PREPARE_CACHE_DIR` and serializes
  writers in host state by canonical remote identity, so N worktrees share one
  package-manager store. A step can opt in to typed paths with
  `shared_package_caches = ["pnpm", "npm", "uv", "pip"]`; each exposes only
  its corresponding `AETHYME_PREPARE_*_DIR` variable beneath the same
  repository- and digest-specific store.

Prefer `repository_shared` for a package manager with a content-addressable
store (pnpm, uv). For copying package managers such as npm or pip, only their
download cache is shared, so each worktree still materializes its own install
tree. Aethyme does not infer how a command uses a store: add the typed name to
`shared_package_caches` and route the manager explicitly in that step's
command. The broker exposes only its reserved `AETHYME_PREPARE_*_DIR` path;
it does not set or override package-manager environment variables. For
example, a uv command can use
`UV_CACHE_DIR="${UV_CACHE_DIR:-$AETHYME_PREPARE_UV_CACHE_DIR}" uv sync` to
preserve a caller-provided cache location.
The broker creates selected alias directories under the bound cache root and
refuses symlink or non-directory leaves before it starts the command.

`CARGO_HOME` is not a cache-only directory: it also holds configuration,
credentials, and installed executables. It is deliberately not a supported
shared cache and the broker leaves it untouched. Keep writable build outputs
such as Cargo's `target/`, `node_modules/`, and Python virtual environments in
each worktree.

The shared store is keyed by preparation digest, so a repository whose lockfile
differs between branches accumulates one store directory per distinct input set.
`shared_cache.max_bytes` defaults to 8 GiB and is a pre-run rotation threshold,
not a write quota. Rotation happens inside the exclusive lease, never after it,
because it removes a store other worktrees may be reading. Declaring a budget
without a `repository_shared` step is refused rather than ignored.

By default one preparation at a time uses a repository's shared store, so a
second session waits for the first to finish its steps. A repository whose
shared steps tolerate another copy of themselves writing the same directory
can let several run together:

```toml
[shared_cache]
max_concurrent = 3
```

`max_concurrent` accepts 1 to 6 and defaults to 1. It is a statement about the
repository's own commands, which Aethyme cannot check: package-manager download
and content-addressable stores (pnpm, uv, pip, npm) are written for concurrent
use, a step that builds into the shared directory in place usually is not.
Sessions preparing the same digest share one directory, so that is the case to
reason about. Work that moves or removes the store still runs alone: finishing
an interrupted rotation, the budget check and rotation, the cache sweep and
cache-root initialization each wait for every running preparation, and nothing
starts while they hold the store. With sharing on, the budget is checked at
most once an hour, by a preparation that finds the store free; when a check is
a day late, preparations wait for the store alone until one has run it. Every
broker on the host must be 0.8.69 or later before a repository sets this: an
older broker refuses the unknown setting, and its own sweep does not wait for
sharing preparations. It cannot be combined with `managed_storage`.

Finishing an interrupted rotation survives a remount. The host journal remembers
each preparation-cache root by its device and inode, and macOS gives an external
volume a new device number on every remount, so a journal row saved before a
remount used to stall recovery for good ("preparation cache root identity
differs from its durable journal"). Recovery now records the root's volume UUID
and, when the device number alone changed, resumes on its own after proving the
root is the same directory: the inode is unchanged, no rotation debt is pending,
and either the volume UUID recorded for that row matches or, for a row written
before UUIDs were recorded, the cache root's parent marker, whose random nonce
names this root, was written under the same old device number. Anything else
still stops with `root_volume_reproof_refused: <condition>` in the stop reason;
Linux keeps the exact device comparison because it has no stable volume
identifier here. See [cross-process consumers](../architecture/cross-process-consumers.md).

An unspecified `shared_cache.root` keeps the historical Host placement for
existing configurations. A new pnpm draft from `aethyme init` selects the
worktree placement filesystem and passes its broker-managed store path with
`--store-dir`, so pnpm can hardlink package content when its import mode allows
it. Worktree outputs such as `node_modules/` remain per-worktree. The typed
`pnpm` alias resolves to the same `pnpm-store` directory used by the existing
`AETHYME_PREPARE_CACHE_DIR/pnpm-store` convention. A
worktree-relative `store-dir` in `.npmrc` creates a private store for
installs that do not pass an explicit store path; remove it to avoid that
per-checkout cache:

```toml
[shared_cache]
root = "worktree_volume"
```

The broker derives a repository-owned cache root under the selected worktree
placement root and verifies its ownership marker and filesystem before use.
An explicit `AETHYME_PREPARATION_CACHE_ROOT` operator setting keeps precedence;
the broker refuses that setting for this mode if it is on another filesystem.
Set `root = "host"` when a new configuration needs the historical placement.
Changing the selected placement does not migrate or remove an old cache root.
Only the package-manager store can share physical data this way; declared
worktree outputs such as Python virtual environments remain per-worktree.

`npm install` is never drafted. It rewrites a committed lockfile, which is a
tracked-file mutation the worktree guard refuses without an explicit lease.
Commit `package-lock.json` and the draft uses `npm ci`.

## Tune Git For Repeated Worktree Status

Linked worktrees already share the repository object database. Avoid cloning
the repository again or configuring object alternates for broker sessions:
those add lifecycle and portability risks without removing work Aethyme is
already sharing.

For a large checkout, benchmark Git's filesystem monitor and many-files
defaults against the broker's actual command mix before changing configuration.
Use a clean and a dirty worktree, include ignored build output and untracked
files, and compare several warm runs. `GIT_TRACE2_PERF` records Git's internal
timings when a command remains slow:

```bash
GIT_TRACE2_PERF=/tmp/git-status.perf git -C <worktree> status --porcelain=v1
git -C <worktree> -c core.fsmonitor=true status --porcelain=v1
git -C <worktree> -c feature.manyFiles=true status --porcelain=v1
```

On macOS and Windows, `core.fsmonitor=true` opts into Git's built-in
filesystem-monitor daemon. It can reduce repeated filesystem scans, but has
startup and maintenance cost and may lose on small or mostly clean checkouts.
It is unsupported on some filesystems, including network mounts by default.
The setting is shared by linked worktrees when written to repository config;
all Git clients using that checkout must understand the boolean form (Git
2.36 or newer).

`feature.manyFiles=true` enables Git's many-files defaults: index v4, the
untracked cache and skipped index checksums. It can help status and index
writes, but measure first. The untracked cache assumes reliable filesystem
modification times. Older Git versions may reject or misreport indexes using
skipped checksums; keep all clients on a supported Git version before enabling
it in shared repository configuration.

Only persist a setting after it wins on representative worktrees. Repository
local settings affect every linked worktree and IDE using the common Git
directory:

```bash
git -C <main-checkout> config --local feature.manyFiles true
git -C <main-checkout> config --local core.fsmonitor true
```

Undo an opt-in with `git config --local --unset <key>`. These settings are
machine-local and are not part of `.aethyme/config.toml` or the repository.
Do not enable `feature.experimental` as a performance shortcut: it bundles
changing fetch, pack and new-repository ref-format defaults, so evaluate those
individually with compatibility and migration plans.

## Reuse A Bounded Artifact Cache

For compilers or package managers whose artifacts are safe to reuse across
worktrees, declare a broker-owned cache by logical key and byte budget:

```toml
[[gate]]
name = "rust-workspace"
command = 'CARGO_TARGET_DIR="$AETHYME_GATE_CACHE_DIR" cargo test --workspace'
resource_wait_seconds = 600

[gate.managed_cache]
key = "rust-workspace-v1"
max_bytes = 12884901888
```

The key is not a path. Aethyme derives a private platform cache directory,
scopes it by canonical repository identity, exports it as
`AETHYME_GATE_CACHE_DIR`, and automatically adds an exclusive host lease.
Before each executed run it measures stored file bytes and atomically rotates
an over-budget cache while still holding that lease. JSON gate outcomes expose
the key, budget, bytes before and after, and whether rotation occurred; they do
not expose the absolute host path.

Use a new key when intentionally resetting cache compatibility. Do not embed
`$HOME` cache paths directly in commands: those paths have no broker ownership,
budget, provenance, or cross-clone serialization.

## Install An Opt-In Pre-Push Adapter

Aethyme does not install or replace `pre-push`. Commit this repository-owned
hook (or call the same command from the repository's existing hook manager):

```sh
#!/bin/sh
set -eu

if command -v aethyme >/dev/null 2>&1; then
  exec aethyme broker advanced gates pre-push "$@"
fi

# During a staged rollout, this fallback must allocate unique namespaces and
# dynamic ports itself. Never use a fixed shared port or container name.
if [ -x ./scripts/pre-push-isolated ]; then
  exec ./scripts/pre-push-isolated "$@"
fi

echo "pre-push validation needs Aethyme or ./scripts/pre-push-isolated" >&2
exit 1
```

Git passes the remote name and URL as arguments and its ref-update protocol on
stdin. The adapter preserves stdin, proves that all non-deletion updates point
to the same clean checked-out `HEAD`, and then runs every configured gate. It
refuses dirty checkouts, a pushed non-`HEAD` ref, or multiple different local
tips because one checkout cannot truthfully prove those trees. Run such pushes
from a clean worktree at each intended tip. Deletion-only pushes have no
outgoing content tree and skip content gates.

The normal cache is safe because its key includes the exact Git tree and the
complete gate definition, including resource declarations. Set `cache = false`
for gates whose external dependencies make old evidence unsuitable, or use:

```sh
aethyme broker advanced gates pre-push "$@" --no-cache
```

`--no-cache` bypasses lookup once and stores the new result normally.

## Availability And Fallback

There is no broker service to contact: installed Aethyme processes coordinate
through the per-user registry. If the binary is missing, a repository may use
the explicit safe fallback shown above. Once Aethyme starts, an inaccessible
registry, unavailable resource bundle, or lost renewal authority is a hard
failure; do not launch a second fallback after partial execution.

A fallback is safe only when it independently provides unique namespaces,
dynamic ports, exact ownership, and narrow cleanup. If it cannot allocate a
required resource safely, it must fail. It must never fall back to a shared
port, broad Docker name, or cleanup command that can select another run's
containers.

## Inspect And Recover

Use the low-level resource commands for diagnosis or non-gate integrations:

```bash
aethyme broker advanced resources plan request.json --json
aethyme broker advanced resources acquire request.json --wait 30m \
  --grant-out "$private_runtime/grant.json" --json
aethyme broker advanced resources list --json
aethyme broker advanced resources list --all --json
aethyme broker gc reap --json
aethyme broker advanced resources reconcile <lease-id> --confirm <generation>
```

`plan` and `list` are read-only. `acquire`, `renew`, and `release` remain for
advanced clients using the versioned JSON request/grant contract. `--wait`
accepts bounded `ms`, `s`, `m`, or `h` durations. Timeout JSON uses stable
`resource_contention`; a conflicting active capacity limit uses the distinct,
non-retryable `capacity_policy_mismatch` code. `--grant-out` refuses overwrite,
publishes atomically with mode 0600 on Unix, and omits the ownership token from
stdout. Inventory and gate reports never contain that token, file contents,
diffs, or absolute worktree paths.

### Waiting order

A request that meets ordinary contention while it waits (`--wait` above zero,
`resources run --wait`, or a gate's `resource_wait_seconds`) records a waiter
row in the host-resource store and is served in arrival order per resource:

- A waiter reserves a pool or key only once that resource has been contended
  for it (the pool did not fit, the key was held, or an earlier reservation
  was in the way); resources of its bundle that were free stay usable by
  others. A reservation lasts for the rest of the wait. While any conflict is
  on a TCP port, the waiter keeps its place in line but reserves nothing.
- A capacity request is granted only if its pool has room for what is
  allocated plus the units reserved by every earlier live waiter, plus its
  own, measured against the smallest limit any of them declared. A later,
  smaller request can pass an earlier one only when it cannot take a unit the
  earlier one reserved, so a full-pool request is never starved by smaller
  ones.
- An exclusive key is not granted to a request while an earlier live waiter
  has reserved it.
- A request that does not wait (`--wait` absent or zero) is admitted as if it
  arrived after every live waiter: it fails with retryable
  `resource_contention` instead of taking what a waiter needs.
- TCP ports and namespaces are not queued; the operating system, not the
  store, decides whether a port is free.

The waiter row is removed on grant, on a refusal waiting cannot resolve, and on
timeout. A row whose process is gone, whose own deadline has passed, or whose
heartbeat is older than two minutes stops gating anyone at once and is deleted
by the next acquisition; no reconcile is needed. Order is the row's
`AUTOINCREMENT` ticket, so a clock step cannot reorder the queue, and a waiter
whose row was reaped while it still polled re-enters with its original ticket.
The wait deadline and the row's deadline use the same wall clock, so a process
suspended past its deadline ends its wait instead of re-entering. Short
in-product acquisitions that used to make one non-waiting attempt (preparation
cache root initialization, local execution admission) now wait up to five
seconds in the queue. Between attempts a waiter
reads only SQLite's `data_version`: the head of a queue retries as soon as the
store changes and at least every second, and waiters behind it retry at most
once a second on a change and back off to five seconds without one.

`explain --json` adds a `queue` object (schema
`aethyme.host-resource-queue/v1`): `queued`, `position` (live waiters ahead on
any queued resource it demands; 0 is the head), `enqueued_at`, `waited_ms`,
`times_skipped`, and per-resource `resources[]` with `kind`, `resource`,
`waiters_ahead`, `waiters_behind`, `units_ahead`, `units_requested`,
`units_allocated`, `limit`, and `holders[]` (`lease_id`, `generation`, `units`,
`holder_pid`). Contention JSON from `acquire --json` and `run --json` carries
the same `queue` object at refusal. Human `list` prints the waiting requests in
arrival order, marking which demands each one currently reserves;
`list --json` keeps its lease-array shape. Command metrics gain
`resource_queue` with `waited_ms`, `attempts`, `poll_ticks`, `queued`,
`position_at_enqueue`, `times_skipped`, and `reaped_dead_waiters`.

Older binaries keep working on a store that has the waiter table: it is
created additively and the store version is unchanged. They do not read it, so
their polling acquire can still take units an earlier waiter has reserved;
the queue bounds only current binaries and becomes complete once every running
process has been upgraded.

`reap` is an independent dead-holder sweep. It quarantines active leases whose
holder PID is provably gone, releases ordinary leases' capacity allocations,
and reports the reclaimed units. A supervised capacity hold is retained even
when the supervisor PID is gone, because the child or cleanup process group may
still contain writers. Named allocations such as namespaces, ports, and
exclusive keys also remain quarantined until an operator reviews cleanup and
uses the exact generation-fenced `reconcile` command. Before reconciling,
review independently retained operator run records and the current process
list for surviving writers, then clean exact owned resources. The host-resource
registry records the lease and holder PID, not child or cleanup process-group
IDs.

A minimal low-level request is:

```json
{
  "schema_version": 1,
  "request_id": "unique-attempt-id",
  "repository": "owner/repository",
  "worktree_fingerprint": "opaque-non-path-digest",
  "run_id": "unique-run-id",
  "ttl_seconds": 300,
  "resources": [
    {
      "key": "docker_project",
      "kind": "namespace",
      "prefix": "my-project"
    }
  ]
}
```

For a generic acquisition, reuse a `request_id` only to recover the same
idempotent grant. A `resources run` request ID is one-shot and cannot be reused
to start another supervised child; give each run a fresh ID. Reusing an ID with
different request content is refused. Pass the exact grant JSON to renew or
release; a generic release cannot clear a supervised capacity hold. Do not
place an ownership token in process arguments or logs.

For quarantined Docker or database resources, inspect the recorded allocation,
clean only exact container IDs or the exact allocated namespace, then confirm
the current generation. Never reconcile first and clean broadly afterward: a
new owner may legitimately receive that resource once quarantine is removed.

## Concurrency Guarantees And Limits

- Bundle acquisition and capacity accounting are transactional across clones.
- Expiry generations fence stale renew, release, and cleanup attempts.
- A TCP port is checked with the operating system before allocation, but the
  generic gate contract cannot hold the socket while an arbitrary application
  binds it. Applications must still fail clearly if a non-Aethyme process wins
  that short handoff race. Socket activation would require an application-
  specific protocol and is outside the generic broker contract.
- The registry is per OS user. Different user accounts need an explicitly
  shared coordinator if they intentionally share the same host resources.
