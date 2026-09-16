# Upgrading to Aethyme v0.7.21

Last Updated: 2026-09-16

v0.7.21 is a maintenance release covering broker storage reclamation, session
cleanup, gate classification, and status reporting. Runtime behaviour and
broker storage compatibility are unchanged from v0.7.20.

The theme is measurement: several of these corrections are not new mechanisms
but existing ones that were consulting the wrong reference, and therefore
answering a question nobody asked.

## What is new

### Cleanup can now release worktrees whose branch was squash-merged

A session whose work reached the default branch through a squash merge stayed
uncleanable. Its commits are unreachable by ancestry, and the recorded
representation search resolved the *local* default branch, which the ship lane
deliberately never moves -- so on a machine that publishes through the broker,
that search examined a reference stale by exactly the work it was looking for.

Two corrections. The search now prefers the remote-tracking default branch when
it is strictly ahead of the local one. And reachability from a remote-tracking
ref is accepted as durability: what cleanup needs to know is whether the work
survives the directory, and a pushed ref answers that whatever the merge
strategy did.

The durability evidence is guarded: the ref must still match what the remote
reports, and not knowing -- offline, slow, unreadable -- stays unproven rather
than absent. An offline cleanup behaves exactly as before.

A worktree detached on a commit other than its session branch is no longer
refused outright. It is released when *both* the worktree head and the branch
tip are proven durable, and only then.

### The artifact sweep now makes progress

The autonomous sweep is budgeted for a slice of work and designed to resume,
but it persisted only a cadence timestamp. Every interrupted pass restarted at
the lowest session id, so a budget smaller than one full scan serviced the head
of the list forever. It now records the session it stopped after, and a
completed lap clears the cursor.

### A full disk is no longer recorded as a test failure

A cargo gate whose *test* exhausted the volume was classified `test_failure` and
cached against the tree hash, so every resubmission of the same tree replayed a
verdict that had nothing to do with the diff. Storage exhaustion is now
recognised before the cargo-specific branch of the contention check, whatever
ran into it.

### `--queue-timeout` bounds the whole admission

The flag reached only the repository write lock. The preparation in front of it
-- remote resolution, a pre-push dry run that contacts the network -- carried no
deadline, so a wedged remote hung both `--queue-timeout` and `--no-wait`. The
budget is now shared across admission, and exhaustion raises a distinct
`AdmissionTimedOut` rather than a lock-busy error that names a holder which
never existed.

### `broker status` counts against the published branch

The integration lead was measured against `head_commit()` -- whatever branch
happens to be checked out. On a repository whose checkout sat on an old feature
branch this reported a lead of several hundred commits over something nobody
would publish to. Status now resolves a publication baseline, reports the ref
that answered, and shows the checkout separately when it differs.

## Compatibility

- The broker database schema remains 39; no database migration is required.
- The engine protocol remains version 1.
- v0.7.21 is compatible with the v0.7.20 repository and broker database
  layouts. Installing it alongside older sessions is safe precisely because no
  migration runs -- unlike the 0.7.19 to 0.7.20 window, live sessions are not
  locked out here.

## Before upgrading

Nothing is required. The schema is unchanged, so there is no quiet window to
wait for and no session to drain first.

If you want a baseline to compare against afterwards, record what cleanup
currently refuses:

```bash
aethyme broker cleanup --all-cleaned
```

Several of those refusals are expected to disappear once the new binary can
prove durability it previously could not.

## Install or update

```bash
cargo install --locked --path packages/aethyme/rust/crates/aethyme-cli
cargo install --locked --path packages/aethyme/rust/crates/aethyme-engine
```

Both binaries must come from the same revision: the router and its engine
sibling are validated as a pair, and a mismatched pair is refused rather than
run.

## Migrate and verify

There is no migration. Verify the pair instead:

```bash
aethyme --version
aethyme-engine-cli --version
```

Both must report `0.7.21`. Then confirm the broker still opens its database and
reads its own state:

```bash
aethyme broker status
```

The status header now names the reference the integration lead is counted
against, and shows the checked-out branch separately when it differs. A line
reading `Baseline: refs/remotes/origin/<branch>` is the expected shape.

## Rollback

Reinstall the previous pair from the v0.7.20 tag:

```bash
git checkout v0.7.20
cargo install --locked --path packages/aethyme/rust/crates/aethyme-cli
cargo install --locked --path packages/aethyme/rust/crates/aethyme-engine
```

Rollback is safe in both directions for this release, because the database
schema is identical. No state written by 0.7.21 is unreadable by 0.7.20.

## Known issues

- Reclamation remains repository-scoped while storage is host-scoped: artifacts
  inside a repository's own checkout are outside every automatic lane, and on a
  busy machine they can exceed everything the broker manages. See #195.
- A session holding committed work that was never pushed anywhere stays
  retained indefinitely. That is deliberate -- nothing proves such work exists
  outside its directory -- but it has no automatic resolution, and
  `cleanup <id> --force` after human inspection remains the only exit.
- A promotion whose content landed upstream through a squash *and* whose paths
  were edited afterwards cannot be recognised automatically. Patch ids, content
  comparison and replay all fail on that combination; the information is
  destroyed after the fact and has to be recorded at merge time to survive.
- Integration drift is reported, not repaired. A repository whose integration
  branch has fallen far behind its upstream still requires
  `aethyme broker integration reconcile` with an operator attestation.
