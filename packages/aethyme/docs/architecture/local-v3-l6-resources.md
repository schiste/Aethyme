# Local v3 execution resources (#673)

Last Updated: 2026-10-11

This draft connects the offline Linux executor and CLI verifier to finite Local
budget attempts and the existing `HostResourceCoordinator`. It requires an
appointed resource policy, current exact wave/candidate/execution identity,
remaining envelope and external epoch. Native dispatch keeps the ledger stable
through daemon start acknowledgement. An attempt cannot dispatch a second run.

The native and daemon memory/disk/process/job pools reserve conservative bounds
for candidate limits, helpers, materialization and archive copies. Native disk
floors cover temporary and project filesystems. A separately pinned trusted
capacity-tool image probes only an empty volume created by the run; source,
expected answers, daemon sockets and unrelated host directories are absent.
The first profile uses the default Docker local volume driver without custom
input-volume options. Daemon memory and volume disk are observed before work.
Unmanaged host work can still cause infrastructure failure; capacity admission
is cooperative and does not attest the entire host's available memory.

Resource policies are explicit operator appointments, not candidate input.
Both the host-state parent and temporary root must be private. Host journals
must resolve outside the collaboration project backup directory. The resource
profile needs the operator to select floors and pools appropriate to the host;
the normal gate disk floor remains 8 GiB. This draft does not qualify a smaller
production floor from a synthetic regression configuration.

A private durable journal records intents before Docker creation and exact
acknowledged identities before use. It stays outside collaboration backups and
holds a per-run reader/cleanup lock. Cleanup checks exact identity, endpoint,
physical daemon and generation before removing containers and their volumes.
Only that inventory can be removed; retained CAS and source records are outside
its deletion interface. Confirmed cleanup is idempotent. Unacknowledged creation
requires explicit quiescence reconciliation and keeps capacity reserved.
Host leases have no holder PID: a client exiting does not prove its Docker jobs
are gone. Recovery after an approved native binary replacement can clean the
same physical daemon without upgrading historical behavioral evidence.

Native accounting charges the reserved conservative resource envelope after
confirmed cleanup. It is not an invoice or peak-memory measurement. Uncertain
execution/teardown keeps budget consumption pending or unknown. Resource recovery
does not automatically reconcile a budget receipt or publish/accept source.

Cross-candidate result reuse remains disabled. The diagnostic future cache key
includes exact execution and artifact, native producer generation/policy,
harness/collection, authority epoch, target revision and result class. Immutable
execution identity covers configuration/dependencies/dataset/toolchain inputs.
There is no writable shared execution cache or uploaded pass upgrade.

Six focused regressions pass on the initial draft. Live resource/cost schedules,
interrupted cleanup, dispatch races, deliberate guard removal, independent
review and final CI are pending. Older supervisor/verifier component schedules
use explicit `cfg(test)` profile-only paths and do not claim resource evidence.
Production has only admitted execution paths. Full Q3/Q4 qualification, E1
composition evidence and candidate-to-gate wiring remain dependent work.
