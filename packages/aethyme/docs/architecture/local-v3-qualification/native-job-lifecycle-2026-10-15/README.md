# Native job lifecycle qualification checkpoint

Recorded 2026-10-15 for explicit native-job root release and retention.
Production and test changes have independent static review, but no complete
local native-gates command has passed.

## Reviewed source

- Production review clear: d6518745a1df9d75dc77ab2e44af35d5ef3ef470,
  tree 88bf1707e1bac7925e8b453e5f676c11c4ef8dda.
- Typed StoredJobRow mapping and independently selected fixture/spec roots
  reviewed: 1a799a3eead55b9ed2380661c517d2ab0a8b4ce5,
  tree 128a07026ffd8ccf2e3215b4ddfaff6362082949.
- Complete fixture-derived closure oracle reviewed clear:
  d524f3e695baa26a14302e68e3040fc61cef61d0,
  tree 8b8302ffd16f6893bcd0fc8e67478b22728f3fa2.

These are review results, not runtime qualification.

## Attempt history

At 622c633eb9c34b762ba795d2edf2b604b5b29c5b, tree
be1a2a78620362549570002e1020f777babc22f5, CI reported an E0382 moved-value
compile error before tests and a separate rustfmt failure. At
346001298a2db1467b2807610dd04006f4b76889, tree
d758e054309073ef99deb46360e362de98fb2271, the 15-test native_gates filter
returned 14 passes and one overbroad dispatching/unknown GC assertion failure.
Strict Clippy also rejected the retained-row tuple. Full CI logs and local
recorder files remain retained; ledger.json binds exact sizes, hashes and
T7 paths.

At full-closure source d524f3e695baa26a14302e68e3040fc61cef61d0, the same
15-test filter began. Fourteen tests returned ok; the
budget_capacity_and_requirement_holds_do_not_dispatch_prepared_jobs test
did not return before the process was stopped while temporary SQLite fixtures
were using host Data storage near capacity. Cargo recorded status 101; source
remained unchanged. This attempt is incomplete and receives zero qualification
credit.

Across both local commands, 30 test slots were scheduled: 28 returned ok,
one returned an assertion failure in the earlier test oracle, and one did
not return before SIGTERM. No complete local command passed. CI compile,
format and Clippy checks executed no tests. The complete denominator is in
ledger.json.

At d524f3e695baa26a14302e68e3040fc61cef61d0, full workspace CI ran 4,557
cases: 4,548 passed, 9 failed and 31 skipped (exit 100). Seven failures are
old-schema fixtures that leave the schema-23 `roots_released_ms` column in a
current database before rewinding its version; the schema-18 fixture must
retain schema-18 native-job tables and remove only the schema-23 column and
trigger. The other failures are the CLI help snapshot and a discarded-results
ratchet (144 sites versus 143). The raw CI log is retained and hash-bound in
ledger.json.

A cached serial run of all 15 d524 native-gates tests used the previously built
binary (SHA-256 `4b2376a7f628100d52b2b28d0f60511917c42a7d306a7eb88a6d4f599c8b6945`).
It returned 1 pass and 14 failures because its per-run TMPDIR was placed inside
the checked-out Git worktree, so temporary repositories were rejected as
nested. This is an environmental preflight failure and receives zero test
credit. Its complete command, status and raw output are preserved in the
ledger. A rerun is pending with a unique T7 temporary root outside all
worktrees.

## Remaining checks

Pending checks are the cached serial native-gates suite, migration/state
adoption and floor, touched CLI lifecycle/help, controller/target/GC
regressions, ratchets, strict Clippy, and exact guard-removal checks with
source restoration. No guard mutation or final native qualification pass is
claimed here. Append later results to ledger.json with source commit/tree,
numeric status, complete denominator and raw artifact hashes; retain all
failed and interrupted attempts.

Raw CI and recorder outputs remain outside Git under the listed T7 target
directories. Their hashes bind this ledger to the complete outputs; raw files
were not normalized or rewritten.
