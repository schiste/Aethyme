# Exact Local identity: retained qualification attempts

This directory preserves command-level evidence for the Local project binding,
immutable contribution revision, exact context, and bounded identity GC work.
It is not final whole-plan qualification. No held-out or provider trials were
run by these commands.

The complete file and attempt inventory is [inventory.json](inventory.json).
Each attempt keeps the exact argv, source commit/tree/diff identity, execution
environment, full output, numeric exit, and source-restoration result.

## Passing checks

On merged source `528358823fed9eefbec156b92e146da50cfd6304`:

- Exact immutable context: 7 tests passed. This includes selected-only dependency
  gaps, byte budgeting, hidden/absent equivalence, stale rationale retries,
  visibility changes, ancestor retention, and an unrooted exact pin during grace.
- Exact context and operation Brief CLI: 5 tests passed.
- Public contribution revision CLI: 5 tests passed, including explicit Rename,
  exact dependency updates/clear, clone equivalence, distinct capture operations,
  rationale-only revisions, and malformed input refusal.
- Replay map allocation budget: 1 test passed.
- Bounded identity GC discovery: 4 tests passed after exact restoration.

All 22 tests passed with zero ignored tests and no compiler warnings. This source
includes parent `9dde57f0f4f71552e35ff96ce47110f46240d494`; it does not include the
later target/wave/peer bridge merge.

On combined source `18913101e3ec1a25ad44dac47803fe91aa9e9937`, all 45 contract
integration tests passed, and strict Clippy passed for the contracts package.

## Guard controls

Unique backups and the exact drivers are under [control-sources](control-sources/).
The older two controls each failed their intended assertion on source `bfd90a24`.
After independent review, three controls on source `e38defb9` separately removed:

1. Cumulative live-root row admission: the test failed when iteration continued
   past the first refused root.
2. Charging failed manifest reads: the test failed because bytes remained after
   a corrupt read.
3. Streaming native-root enumeration: the actual SQLite query plan contained a
   temporary deduplication tree and the test failed.

Each control executed one test, failed at the intended assertion with exit 101,
and restored exact source bytes. The restored four-test GC suite passed in the
merged-source run above. These finite controls do not establish universal
resource bounds or reliability for other profiles.

## Retained failures and uncredited selections

- The first exact-context build failed before tests because of an undeclared
  decoder dependency; the fix uses the existing fixed-size digest parser.
- The first six-test context run passed five and failed the ancestor proof-edge
  assertion. The parent writer now persists those edges; the later seven-test
  run passes both retention and retirement paths.
- Strict broker Clippy on `52835882` failed on a context test clone and placement
  of a parent test module. The clone has since been fixed; parent cleanup and a
  passing combined rerun are pending.
- The contracts `--lib` and nonexistent project CLI module selections each ran
  zero tests. They carry no test credit. The corrected contracts integration
  command executed and passed 45 tests.
- The first bridge consumer build on `18913101` stopped on seven test compilation
  errors. No consumer tests executed; fixes and runtime qualification are pending.

The inventory retains earlier positive runs as historical attempts without
extending their claims to later source. Final workspace checks, complete public
CLI rehearsal, schema-floor compatibility, and combined consumer qualification
remain separate work.
