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

Implementation and qualification are in progress. These descriptions name the
intended boundary; they are not Q3 or E1 composition evidence.
