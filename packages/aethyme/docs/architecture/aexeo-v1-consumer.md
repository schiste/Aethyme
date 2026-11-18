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

`content_gate::strict::strict_precheck_v1` additionally validates both JSON
documents against the bundled, unchanged Aexeo v0.0.24 v1 schemas with format
checks enabled, rejects duplicate keys and oversized documents, verifies check
summary counts and compares independently supplied decision input snapshots.
The host provides policy, options, collection and external observations
explicitly and names every additional input. The bundled schemas and unique-key
parser derive from Aexeo; their MIT license is included alongside them.

The host must collect the inputs from trusted sources actually used by the
validator and prove its declared inventory is complete. Comparing two supplied
snapshots cannot establish those facts, validator authenticity, source freshness,
collection completeness, or atomicity with CMS publication. This API does not
run a process or expose a gate command, and its successful result is not a
publication authorization. A runner must supply actual exit/timeout evidence,
ensure the verdict belongs to that invocation, prevent generic gate cache
reuse and perform its final comparison at the publication boundary.
