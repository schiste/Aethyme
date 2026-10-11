# Local v3 independent verifier (#672)

Last Updated: 2026-10-11

The first proposed profile is `aethyme.verify.offline-cli/provisional-v0`.
It checks the actual sealed executable artifact through fresh isolated CLI
invocations. A native harness outside the candidate compares observed exit,
stdout and stderr with a nonempty, retained assertion inventory. The candidate
receives only the current trial input; it receives neither expected answers
nor a writable evidence channel. Candidate-uploaded JSON, added tests and build
exit codes cannot construct the native evidence capability.

The authority selects an exact producer policy, image/runtime, command,
harness and inventory. Producer rotation and revocation use explicit expected
generations. Evidence binds current target/obligation revision, candidate,
execution, sealed artifact, runtime, harness, inventory and authority epoch.
Expiry and current producer trust are rechecked. Historical observations stay
historical after revocation, without granting new acceptance authority.

The profile supports offline command behavior only. It makes no UI/browser,
network service, deployment, arbitrary-language or universal-correctness claim.
Required UI behavior needs an independently controlled browser profile.
Declared but unused preparation/dataset profiles are refused. Cross-candidate
result reuse remains disabled; actual host/budget coupling and interrupted
cleanup recovery belong to #673. Gate wiring still waits for #725.

Collaboration migration 11 adds the local producer and evidence journals and
raises the compatibility floor to 11. Quiesce older open clients. The existing
external-authority checkpoint includes all `local_*` tables, so announced
restore covers these journals without silently reviving old producers.

Independent adversarial review is clear at `3d95c68d`. Eleven restored verifier
regressions pass, including the real pinned-Debian Docker schedule. Fifteen
reachable guard removals each fail their intended regression. The assertion
inventory also inherits the canonical record decoder's stricter 1 MiB document
limit; its later 4 MiB check is redundant and has no independent guard-removal
claim. Host/resource and complete Q3 qualification still depend on #673 and
the Local QA package. None of this is E1 composition evidence.

The runtime descriptor hashes the native executable path and configured Docker
client path. Deployment must keep both installations immutable while a process
is active, and restart the supervisor after either changes. These path digests
are not independent attestations of loaded process memory; the host and its
administrator remain trusted. See [Rust current_exe](https://doc.rust-lang.org/std/env/fn.current_exe.html).

Only exit-zero assertions can qualify in this first CLI transport profile.
Docker's attached client can report a nonzero workload exit or an attachment
failure with the same code, so nonzero completion is not a separate transport
witness. Such expected assertions are unsupported; other ambiguous observations
are infrastructure-deferred. See [pinned Docker CLI start semantics](https://raw.githubusercontent.com/docker/cli/v29.8.0/cli/command/container/start.go).

Receipt issuance checks cancellation inside its SQLite transaction before the
journal insert and immediately before commit. Cancellation seen there rolls
back the receipt; orphan CAS data does not satisfy the authority journal.
The issuance linearization point is the final precommit cancellation read.\nCancellation arriving after that read can overlap commit and does not rewrite\nhistorical data; future
applicability still requires the current target, producer and epoch.
