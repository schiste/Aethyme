# Concurrent Host Resource Coordination

Last Updated: 2026-08-26

Aethyme can run the same repository's validation gates concurrently from
independent clones without sharing fixed ports, Docker project names, database
names, or host-capacity slots. The repository still defines what validation
means in `.aethyme/gates.toml`; the broker only allocates declared host
resources and executes each gate locally.

No daemon or central runner is required. A per-user SQLite registry coordinates
all Aethyme processes on the host. Its default location follows the platform's
user state convention and can be overridden with `AETHYME_HOST_STATE_DIR` for
tests or an explicitly isolated environment.

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
  exec aethyme broker gates pre-push "$@"
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
aethyme broker gates pre-push "$@" --no-cache
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
aethyme broker resources plan request.json --json
aethyme broker resources list --json
aethyme broker resources list --all --json
aethyme broker resources reconcile <lease-id> --confirm <generation>
```

`plan` and `list` are read-only. `acquire`, `renew`, and `release` also exist
for advanced clients using the versioned JSON request/grant contract. Keep the
grant file private: it contains the ownership token. Inventory and gate reports
never contain that token, file contents, diffs, or absolute worktree paths.

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

Reuse a `request_id` only to recover the same idempotent acquisition. Reusing
it with different content is refused. Pass the exact grant JSON to renew or
release; do not place its ownership token in process arguments or logs.

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
