# Local v3 frozen Rust/TypeScript parity

This eval-side qualification package compares small TypeScript adapters with
the existing Rust Local v3 canonical JSON, record, source snapshot, and
decision brief contracts. Both implementations consume the same tracked,
independently authored golden JSON vectors. The TypeScript code is a bounded
qualification aid; it is not imported by the Rust product or presented as a
production TypeScript consumer.

Run it from the repository root with a Node runtime that supports native
TypeScript type stripping:

    node --experimental-strip-types packages/aethyme-eval/local-v3/t01-canonical-parity/t01-canonical-parity.mts

The runner uses Node built-ins only. It needs no install, network access,
Cargo, or product build. It is read-only: it verifies SHA-256 pins for the
golden JSON files, their Rust test consumers, and Rust contract sources, then
prints one JSON report with case counts, the pinned reference commit, frozen
source hashes, and hashes of the TypeScript sources. A changed pinned source
requires a newly reviewed qualification; the runner never updates pins or
fixtures.

The canonical JSON and record profile vectors cover duplicate keys (including
escaped aliases), safe-integer boundaries and out-of-range integers, null
rejection, absent and explicitly unknown state equivalence, unordered string
sets, equivalent encodings, exact record digests, malformed records, raw path
bytes, snapshot order, and critical path and mode refusals.

The brief profile uses the frozen 30 token cases, all decision-file and record
vectors, and the 27 stored calibration inputs. It checks exact profile counts,
the 149/150/151 boundary, multilingual and escaped/order-equivalent briefs,
field-specific refusal codes, unknown future metadata, record digests, and
the oversized hidden-metadata refusal. The implementation follows the
repository's Apache-2.0 license and adds no third-party package, model
vocabulary, or tokenizer asset.

The report is finite fixture evidence for the covered contracts. It does not
claim general TypeScript interoperability, model-token equivalence, or a
production TypeScript consumer.
