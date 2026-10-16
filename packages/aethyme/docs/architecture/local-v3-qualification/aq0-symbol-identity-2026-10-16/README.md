# AQ0: scoped symbol identity and conservative baseline evidence

Last Updated: 2026-10-16

## Scope

This package records the bounded AQ0 implementation and qualification for the
experimental D39/T66 symbol-mapping boundary and the T78 coarse analysis
baseline. The implementation keeps existing `NodeId` generation unchanged,
uses two actual disposable synthetic Playground Git clones, and resolves only
an explicit mapping claim inside an exact local binding. Unproven correspondences
remain `Unknown`; no serialized contributor claim or matching string can create
an established mapping.

The two-clone fixture covers independent source snapshots and origins, body
edits, path moves, renames and copies, equal local labels in separate producer
or document scopes, historical same-name overload collisions, external package
version mismatches, and malformed or mismatched evidence. Existing NodeId inputs
and outputs are asserted from the actual clone paths. T78 coverage uses the
independent analysis vectors for producer timeout, omitted language, truncation,
unavailable producer, stale views/cursors, and other conservative degradations.
It adds no semantic producer and does not execute optional X1/X3/X4 profiles.

## Source and results

The qualified implementation and focused inputs were tested from clean commit
`025103777c16607639073cbf9024d2ad0c39f72d`, tree
`5f7ef505a452b79b01faa325662102497bcb7bff`. This evidence package is on the
later documentation merge `70c1c715be09d104f98728e4824e1b429b6b1459`; the
runtime remains bound to 02510377. The original code/test byte hashes, exact
commands, per-run counts, tool output, and restoration evidence are in
`ledger.json` and `payloads/artifacts/`.

At the qualified source, the three positive targets passed 13/13 before the six
one-predicate controls and 13/13 again after exact restoration. Each control
produced its intended single-test failure, then verified the original source
hash, newer source mtime, and clean target diff. Scoped strict Clippy and
workspace formatting checks passed. The earlier zero-test shared-cache
permission denial, the earlier 5/6 attempt, the prior strict-Clippy failure,
and other historical attempts are retained with no final qualification credit.

Frozen raw outputs use numeric `.raw` names and are marked `-text` in the
package-local `.gitattributes`. `ledger.json` maps every opaque payload to its
logical source path, media type, exact byte count, SHA-256, source/run binding,
and qualification status. Raw terminal logs retain the original machine paths;
the source commit and file hashes are the portable binding.

## Limits and outstanding work

This package qualifies only the finite experimental mapping adapter and the
listed coarse baseline cases. It does not close T01–T06 wholesale, D10–D12, or
D32. In particular, T03 project enrollment/clone association and T05 generic
FileId enrollment and rename/copy/split/competing-path behavior remain pending;
those tasks are tracked separately. The bounded E1 profile does not supply those
generic identity rules. The TypeScript side remains deferred until a real
consumer exists. No producer integration, semantic completeness, cross-version
compatibility, or authority follows from this evidence.
