# Local v3 Linux supervisor (#671)

Last Updated: 2026-10-11

## Selected provisional mechanism

The profile is `aethyme.docker-linux/offline-v0` on an operator-controlled Linux
Docker daemon with cgroup v2 and the builtin seccomp profile. The native trusted
supervisor runs outside the job. Its runtime identity includes daemon identity,
engine/API/component versions, kernel, architecture and the fixed policy.
Images are exact local content IDs; mutable tags and implicit image volumes
are refused. Runtime drift requires a new descriptor and qualification.

The daemon, Linux kernel and host administrator are trusted. This is container
isolation within the existing Linux VM, with residual kernel/daemon escape and
host administrator risks. It does not claim VM isolation between jobs, root
administrator resistance or universal sandbox security. Remote acceptance
must select an approved producer/profile through D07 and #672.

## Inputs, execution and output

Only archived inputs are materialized. Transfer into private named volumes is
checked against source manifests before those volumes are mounted read-only.
The builder receives source and declared dependencies/configuration; harness
and dataset are withheld. It receives no host bind mount, daemon socket,
canonical credential, evidence key, network, inherited environment, shared
writable cache or candidate-defined healthcheck. A pinned image executable
launches the explicit operator command with an empty environment.

Observed policy includes nonroot UID, dropped capabilities, no privilege gain,
private namespaces, read-only root/input mounts, memory/swap and PID limits.
CPU, file-descriptor, shared-memory and scratch-space bounds are fixed. A
bounded tmpfs output volume is held by a separate read-only container. The
builder's PID namespace must end and its writable mount must be removed before
output is read. A PID1 timeout also bounds workload lifetime after supervisor
loss; uncertain cleanup produces no successful artifact.

Output is read as a tar stream and never unpacked by a host archive extractor.
Only bounded, portable regular files and executable modes are allowed. Paths,
bytes, kinds and modes produce a separate output identity. The supervisor pins
that manifest, file objects and a build-artifact record to the target revision.
A noncloneable sealed-artifact capability is constructed only after observing
execution, stopping writers and completing cleanup. Build success remains an
execution observation, not a behavioral assertion or acceptance receipt.

## Remaining package boundaries

This first profile rejects unwitnessed preparation. Raw materialization is its
only preparation recipe. Independent sealed-artifact validation and mandatory
inventory belong to #672; host capacity, reservation coupling, durable crash
inventory and idempotent cleanup recovery belong to #673. Cross-candidate
result caches remain disabled. No broker schema, queue or gate flow is changed.
E1 composition and #725 typed-candidate integration remain external dependencies.
Q3 and full Local qualification are not claimed from these mechanism tests.

## References

- [Docker runtime controls](https://docs.docker.com/engine/containers/run/)
- [Docker daemon trust boundary](https://docs.docker.com/engine/security/)
- [Volume mount and lifecycle behavior](https://docs.docker.com/engine/storage/volumes/)
- [Local tmpfs volume driver](https://docs.docker.com/reference/cli/docker/volume/create/)
- [Tar entry inspection](https://docs.rs/tar/0.4.46/tar/struct.Entry.html)
