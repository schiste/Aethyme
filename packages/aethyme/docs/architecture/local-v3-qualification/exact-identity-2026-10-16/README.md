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

On combined source `a6b34828cb657364ed255269f056001ff581bc96`, all 23 wave
tests passed, including refusal of competing proposals split across one-item
waves and exclusion of unselected catalog entries. The consumer selection ran
15 tests: 13 passed and two failed. The target profile selection ran six tests:
four passed and two failed. These failed commands do not qualify the consumers.

On merged source `d31ce5c4a78c2090972e12ae397e0a578a304496`, the revision
contract integration target passed all three tests and the descriptor vector
target passed all nine tests. These include the new canonical size/count
admission cases. Broker binding and migration tests did not run: compilation
stopped on two test-code borrow errors. The previous parent checkpoint had
stopped earlier on two error-message format strings. Both failures are retained.
The formatter check on `e47766f0` also retained the existing bridge-format diffs.
Further bounded-allocation and brief-reference validation changes remain under
review and are not qualified by these runs.

On merged source `595082df8f759a3f6c6c30340056d8ef9f93188d`, the eight
public binding tests executed: seven passed and the corrupt-bootstrap case failed
because its typed `corrupt_state` response used generic exit 1 instead of refusal
exit 3. That command remains uncredited. Each of the seven migration selections
passed its one test, including schema-23 capture-row preservation. The current-head
inheritance regression and writer serialization test each passed; the latter still
used a timing-based negative observation and needs the independently requested
deterministic contention control before it can qualify lock contention.

On merged source `9328d987e666fd3d2ac5442c019911722cc4be6f`, all 18 revision
consumer tests, six target-profile tests and 23 wave tests passed with zero
ignored tests. The workspace formatter check passed. These runs include the
bounded chunked transport and corrected aggregate-page fixture. Parent test-helper
warnings remain; constructor hardening and the binding error mapping are still
pending, so these checks do not qualify the final combined source.

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

Six additional controls on combined source `a6b34828` separately disabled
conflicting-pin refusal, the final visibility epoch check, dependency-gap
recomputation, base-proof persistence, exact-pin grace seeding, and cumulative
identity-map allocation charging. Each executed one test and failed at the
expected boundary with exit 101. The base-proof control was caught by the
resolver’s typed corrupt-proof check. Exact bytes were restored from a unique
backup; the restored seven-test context suite and replay-budget test passed.
These runs contain the bridge’s existing unused-field warning.

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
  errors. No consumer tests executed. The compile fixes were merged into
  `a6b34828`; the subsequent actual tests exposed two failures: a revision
  packet exceeded the canonical string limit, and the aggregate-page setup
  exceeded the canonical record limit and reached an unchecked constructor
  assumption. The later bounded transport run above passed; further constructor admission changes remain in progress.
- Two inline target profile tests rejected invalid synthetic IDs before testing
  their intended behavior; the corrected inputs passed independent review and the later six-test
  target-profile run above.
- The retained completed CI jobs include schema downgrade fixtures that kept a
  newly added column, an out-of-date collab help snapshot, formatting/test-module
  lint failures, and dependency-download failures. These are historical failures
  on their recorded heads, not passing final CI evidence.

The inventory retains earlier positive runs as historical attempts without
extending their claims to later source. Final workspace checks, complete public
CLI rehearsal, schema-floor compatibility, and combined consumer qualification
remain separate work.
