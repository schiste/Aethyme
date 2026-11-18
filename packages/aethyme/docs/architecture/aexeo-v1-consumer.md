# Aexeo content-gate v1 subject precheck

`aethyme_broker::content_gate::precheck_v1` provides a conservative building
block for a host consuming a `content-gate.verdict/v1`. It accepts JSON bytes,
the observed process exit and timeout, and the actual candidate bytes keyed by
entity ID. It checks exit 0, pass status, blocking checks, exact change/base and
entry identities, and SHA-256 of the current candidate, including deletion
absence. Duplicate, missing and extra entities are rejected. It accepts failed
non-blocking checks. It has no cache and must not be replaced by the generic
tree-only gate cache when other inputs can change.

A successful `SubjectPrechecked` is **not a pass verdict or authorization to
publish**. The function does not execute a validator, ensure a fresh verdict
file, validate the complete JSON schemas, authenticate the validator, or bind
entry metadata, policy, host options, collection snapshots or external
observations. The host must do all of these, retain and compare all decision
inputs, ensure the check and publication are appropriately synchronized, and
recheck the actual candidate at the publication boundary. Source freshness and
snapshot completeness remain host responsibilities. If any cannot be
established, the required gate must block. No CMS integration is implied.
