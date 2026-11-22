# Aexeo content-gate v1 subject precheck

Last Updated: 2026-11-20

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
summary counts, rejects duplicate check IDs and dangling finding-to-check references,
and compares independently supplied decision input snapshots. These are
conservative runner requirements in addition to the open v1 JSON schema.
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

## Native gate and runner

Configure an always-selected, uncached native gate with a deadline:

```toml
[[gate]]
name = "content-review"
kind = "content-gate-v1"
manifest = "content-gate.json"
timeout_seconds = 60
```

The type rejects an explicit command, `cache = true`, path triggers, managed
caches and a missing deadline. It dispatches `aethyme content-gate run
content-gate.json`. A generic broker command gate cannot run that entrypoint:
the entrypoint requires the broker's native uncached marker when inside a gate.

The manifest is a JSON object with required fields `change`, `policy`, `options`,
`collection`, `external`, `additional`, `content`, `validator`, `required_checks`, `env` and
`timeout_seconds`. Paths are interpreted relative to the runner's current
working directory. `collection` and `external` are paths or explicit `null`;
`additional` maps names to paths for every other input; `content` maps each
entity ID to its candidate filesystem path, including the path that must be
absent for a deletion. `validator` is a nonempty argument vector starting with
an absolute executable path. `env` is the complete environment passed to the
validator (the runner adds the two protocol variables). `required_checks` is a
nonempty array of distinct check IDs selected by the host. Each ID must appear
in the verdict and every matching check must be blocking and passed. Hosts must
update existing manifests to declare these IDs; omitting them fails closed. IDs
are exact verdict check IDs, including Aexeo entity scoping; select them from
the actual preset and candidate rather than copying an example. The validator deadline
is between 1 and 3600 seconds and should be no longer than the gate deadline.

The runner takes fresh input snapshots, hashes the validator executable, invokes
the validator with a private new verdict path and a bounded deadline, then
compares snapshots, validates the verdict and checks current candidate bytes.
Missing or malformed data, process failure, surviving process-group members,
stale inputs and missing verdicts block. The host must ensure its manifest
lists all decision inputs actually used, including transitive files and remote
observations, and must establish trusted validator and input provenance. The
runner cannot guarantee publication-time freshness or atomicity: the publishing
host must re-run these checks on its exact candidate and synchronize validation
with publication. A pass does not publish anything.

For example, a host can provide the following manifest when it has selected
and configured the preset deliberately (adapt paths and validator options to
its actual inputs):

```json
{
  "change": "candidate/change.json",
  "policy": "candidate/editorial.toml",
  "options": "candidate/options.json",
  "collection": null,
  "external": null,
  "additional": {},
  "content": { "article-123": "candidate/article.md" },
  "validator": ["/usr/local/bin/aexeo-cli", "content-change", "--preset", "news", "--preset-source", "news=candidate/editorial.toml"],
  "required_checks": ["gate:11:article-123:news.minimum_words"],
  "env": { "PATH": "/usr/bin:/bin" },
  "timeout_seconds": 45
}
```

The `policy` and `options` fields record files; they do not automatically add
validator arguments. Ensure the validator actually uses the files and options
whose bytes the host records. Include every policy dependency, executable helper
and separately loaded input under `additional` when relevant. If this complete
inventory cannot be established, the result cannot authorize publication.

Equal before/after snapshots do not prove an input stayed unchanged throughout
evaluation: a file could change temporarily and be restored between reads. The
host must supply immutable snapshots, or otherwise prevent concurrent mutation
while the validator reads them, and retain the same protection through the
publication-time check. The runner does not establish that guarantee itself.


## Experimental offline v2 comparison

`aethyme content-gate compare-v2 manifest.json` compares two draft v2 envelopes against independently acquired host inputs. It is forbidden inside broker gates and always reports `publication_authorized: false`. Exit 0 means evidence matched; exit 2 means it did not match or could not be checked. The stable JSON result schema is `aethyme.content-gate-offline-comparison/v1`; it does not authorize publication.

The JSON manifest has path fields `previous`, `fresh`, `base`, `after`, `change`, `resolved_policy`, `host_options`, `evaluator_identity`, and `collection` (a path or explicit null); string fields `validator_name` and `validator_version`; an array of distinct `required_checks`; and `fresh_exit_code` (integer or explicit null) and `fresh_timed_out` (boolean). Paths are interpreted relative to the process working directory. `base` is the host snapshot JSON and `after` is an array of changed item projections. An optional `external_evidence` path supplies the exact source-bound link snapshot for the observed-link profile; omission or null declares it absent. Inputs are size bounded and JSON duplicate keys are rejected.

The command reads files supplied by the caller: it does not run or authenticate the evaluator, establish freshness, prove inventory completeness, or synchronize with publication. In particular the process status in the manifest is a caller assertion. The host must independently establish and protect all inputs, including collection and link observation provenance and freshness. The comparator validates snapshot shape, change identity, source digests, and observation counts and values; it does not independently extract destinations or authenticate the fresh evaluator. Matching digests and extension metadata do not authenticate the snapshot or establish its freshness. Files are read separately and may change during the operation.


## Experimental supervised v2 comparison

`aethyme content-gate run-v2 manifest.json` runs the evaluator to produce a fresh envelope before comparison. This is experimental, forbidden in broker gates, and always reports `publication_authorized: false`. Its manifest supplies `previous`, `base`, `after`, `change`, `resolved_policy`, `host_options`, `evaluator_identity`, and `collection` (a path or explicit null), plus `validator_name`, `validator_version`, distinct `required_checks`, an absolute `validator` argument vector, lowercase `validator_sha256`, a complete `env` map, a `timeout_seconds` between 1 and 3600, and an `additional` map for every other evaluator input. Paths are relative to the current working directory. The validator receives `CONTENT_GATE_CHANGE_FILE` and a new private `CONTENT_GATE_VERDICT_FILE`. The result is JSON schema `aethyme.content-gate-supervised-comparison/v1`; exit 0 means matching evidence and 2 means failure or not checked.

The command hashes the evaluator and compares the manifest, inputs, executable and declared additional files before and after the bounded run. The caller must ensure the executable, arguments, environment and all dependencies actually correspond to the evaluator identity and bound inputs, and include every transitive dependency. Equal snapshots do not detect temporary changes, a restored file or a validator reading different files. Hosts must use protected immutable inputs, independently establish collection completeness, provenance and freshness, and synchronize validation with publication. This command does not enforce that boundary or support external evidence; it never authorizes publication.
