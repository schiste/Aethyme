# Troubleshooting

Last Updated: 2026-11-14

## Explore Returns `degraded`

Explore answers without a graph, but then its `trust_policy` is
`verify_before_use` and `safe_to_use_as_answer` is `false`: the hints are
ranked source-search navigation, not caller or impact evidence. This is the
expected result for a repository that has not opted into the graph. Verify the
spans that `aethyme verify-targets` prints, or enroll the graph (below) when
you need caller and impact answers.

## Graph Commands Refuse: Graph Store Missing

### Symptoms

- `aethyme query symbol`, `task pack`, `task explain` or `graph callers` fail
  with `graph store at .../.aethyme/graph_store.redb is missing`
- `aethyme repo ingest` or `repo inspect` fail with `open fragment store`
- `aethyme graph materialize` refuses with `AuthorityDisabled`

### Checks

```bash
aethyme graph status --repo . --json
```

Graph support is a repository opt-in, and query commands are read-only: they
never build the store. Enroll with `aethyme deploy --repo . --with-graph`,
commit, then follow the [graph refresh guide](graph-refresh.md) to generate the
committed fragments and materialize the local store.

## Broker Refuses: Schema Version Is Newer

### Symptoms

- `broker db schema version N is newer than this binary supports (M); install a released aethyme that supports it`
- agent plugin hooks fail or go quiet on the same machine

The broker database is machine-wide, and migrations are one-way. A newer
binary has migrated it: another worktree's `cargo install`, or -- before
#361 -- a gate's build. Gates now run against a disposable database and are
refused the shared one, so a gate can no longer be the cause.

Recover with released binaries only:

1. Look up schema `N` in [`UPGRADING.md`](../../../../UPGRADING.md). If a
   release introduced it, upgrade every installed copy of the `aethyme` and
   `aethyme-engine-cli` pair to that release (`aethyme update`).
2. If no release contains schema `N`, an unreviewed build migrated the
   database. Do not build or run that branch against the shared database to
   get unstuck: that hands shared state to code nobody reviewed. Stop the
   broker sessions, copy `.aethyme/broker.db*` aside as evidence, and wait
   for the migration to ship in a release -- or move the database aside and
   let the released binary recreate it, re-adopting live worktrees with
   `aethyme broker start --adopt --task "Resume work" --short-name "Resume work"`.

## Aethyme Behaves In Ways Its Version Cannot Explain

### Symptoms

- a command fails against an engine capability its version should have
- behaviour changes between one worktree and another with no code difference
- `aethyme --version` looks entirely reasonable

### Checks

```bash
aethyme plugin status
```

Look at the `Engine pair:` line. `aethyme` and `aethyme-engine-cli` are one
product shipped as two binaries, and on a development machine there is one
`~/.cargo/bin` but many worktrees: whichever session ran `cargo install --path`
last wins, separately for each binary. Two sessions installing from different
branches on the same afternoon leave a router from one beside an engine from
the other.

They usually share a version number and differ only in the suffix `git
describe` adds, which is why reading `--version` alone does not reveal it:

```
aethyme               0.7.16 (v0.7.16-8-gf74bf96b)
aethyme-engine-cli    0.7.16 (v0.7.16-6-g080d3c8a)
```

The fix is to reinstall both from one tree, in one command, so a later failure
cannot leave the pair half-updated:

```bash
cargo install --locked --path packages/aethyme/rust/crates/aethyme-cli \
  && cargo install --locked --path packages/aethyme/rust/crates/aethyme-engine
```

Then confirm both report the same build, and check nothing else on `PATH`
shadows them — `which -a aethyme` shows the order, and a Homebrew copy in
`/opt/homebrew/bin` precedes `~/.cargo/bin` on a default macOS `PATH`.

The `SessionStart` hook reports a split pair to the agent as well, so an agent
session that starts on a skewed machine is told before it does any work. Set
`AETHYME_UPDATE_CHECK=off` to silence that notice.

## Checkout Count Keeps Growing

### Symptoms

- `aethyme broker start` refuses a new checkout at `[worktrees].max_checkouts`
- `aethyme broker status` shows a `cleanup.checkout-capacity` warning
- closed sessions leave their checkouts on disk instead of being removed

Raising `[worktrees].max_checkouts` only delays the refusal and reclaims
nothing. The durable fixes are the `[cleanup] regenerable` setting and the
automatic removal of closed checkouts, which runs under capacity pressure
(from three quarters of the limit) and when `start` reaches the limit. Find out
what keeps each checkout, then fix that cause.

### Checks

```bash
aethyme broker status --json
```

1. Read the `cleanup.checkout-capacity` entry in `advice[]`. Its evidence gives
   the registered count, the limit, the open sessions and the retained
   (closed or unowned) checkouts. If the open sessions are most of the count,
   finish or close the sessions you no longer need; removal only touches
   closed checkouts.
2. Read `capacity_removal.outcome`, the background worker's last removal
   slice. The same record is `auto_cleanup.capacity_removal` in
   `aethyme broker gc plan --json`.

   | `outcome` | Meaning and next step |
   |---|---|
   | `removed` | The slice freed slots and goes on. Check again in a few minutes. |
   | `nothing_removable` | The lap ended and every examined checkout was kept. Go to step 3. |
   | `lap_unfinished` | Time ran out before the lap reached its end. The next slice resumes it after one minute. |
   | `lock_busy` | Another maintenance pass held the GC lock. The next slice retries after one minute. |
   | `inventory_unavailable` | No complete checkout inventory before the deadline. `waiting_on` names what it waited on (`capacity_probe`, `cohort_index`, `group_page` or `live_paths`). Progress is kept; the retry is one minute after a slice that moved, and backs off to ten minutes when it did not. |
   | `out_of_time_before_selection` | The budget ran out before any checkout was judged. `budget_consumed_by` is `checkout_count` or `pre_selection`. It retries after one minute and backs off to ten while it repeats. |

   `lap_remaining` says how many closed-checkout groups the lap still has, and
   `scan_ms`, `selection_ms`, `apply_ms`, `paced_ms` and `elapsed_ms` say where
   the slice's time went. To run a pass now instead of waiting for the worker,
   use `aethyme broker gc sweep --json`.
3. Read `capacity_removal.kept_by_code`, the number of kept checkouts per keep
   code, and `aethyme broker gc plan --json` for the per-checkout reasons in
   `auto_cleanup.kept[]` (`code`, `reason`, and for ignored leftovers
   `ignored_blockers`). Fix the code with the most checkouts first.

### Fix by keep code

| `kept_by_code` key | What it means | What to do |
|---|---|---|
| `ignored_content` | An ignored file or directory that no recognized or configured pattern releases keeps the checkout. | Read `capacity_removal.blocked_by_ignored`. Each row has the ignored `name` (a directory ends in `/`), the number of `checkouts` it keeps and a `config_line`. If the path holds only output its tools regenerate, add that line to `[cleanup] regenerable` in `.aethyme/config.toml` on the default branch. For a standalone file the line names exactly that path; widen it yourself (for example `reports/quality-*.json`) when every match is generated. A row with `config_refusal` instead cannot be released by configuration (a protected name such as `.env`, a key or a database file, or a root name a pattern may not name): review and delete the file, or keep the checkout. See "Automatic removal" in [worktree cleanup](worktree-cleanup.md). |
| `ignored_content_unverified` | The ignored-path check could not finish, usually for time. | Run `aethyme broker gc sweep --json` again; if it repeats, inspect the checkout and use `aethyme broker finish cleanup <session-id>`. |
| `uncommitted_changes` | Tracked, staged or untracked changes. | Commit and push what matters, or preserve it with `aethyme broker finish cleanup resolve <session-id> --archive`. |
| `operation_in_progress` | A rebase, merge, cherry-pick, revert or bisect is unfinished. | Finish or abort it in that checkout. |
| `stash` | A stash was made on the session branch. | Apply or drop the stash, or archive the checkout. |
| `unmarked_tracked_deletions` | Tracked files are deleted and the broker has no record of removing them itself. | Look at the checkout: restore the files, or remove it with `aethyme broker finish cleanup <session-id>` once you know the deletion is not unique work. |
| `strict_tracked_deletions` | An interrupted removal left tracked deletions and `[cleanup] strict = true` forbids finishing it. | Set `strict = false`, or finish the removal with `aethyme broker finish cleanup <session-id>`. |
| `not_contained` | Commits that are not on the fetched default branch. | Land the pull request and fetch; the next pass removes the checkout. For abandoned work, archive it. |
| `containment_unproven` | Git could not prove containment. The reason carries the error. | Fix the Git error the reason shows, then sweep again. |
| `no_remote_default` | There is no fetched `origin/HEAD` to prove containment against. | Fetch the remote default branch. |
| `session_open` | The owning session is still open. | `aethyme broker finish` or `finish close` the session. |
| `in_grace` | The closed-worktree grace period has not passed. | Wait for `[retention] closed_worktree_grace_hours` (24 by default), or remove it now with `aethyme broker finish cleanup <session-id>`. |
| `pinned` | A `[cleanup] keep` pattern matches. | Remove the pattern or remove the checkout by hand. |
| `lease_held`, `gate_running`, `artifact_use`, `process_use` | A lease, a running gate, an artifact-use marker or an open file or working directory is using the checkout. | Wait for the work to end or release the lease. For markers left by a dead process, review `aethyme broker gc artifact-use reconcile plan <session-id>`. |
| `policy_changed` | The cleanup policy or retention changed after selection. | Sweep again; the next pass judges under the new policy. |
| `ownership_witness_missing` | The spawned session never recorded the witness that proves the broker created the directory. | Inspect the checkout, then `aethyme broker finish cleanup <session-id>`. |
| `ownership_witness_legacy` | The witness is a schema-1 record without a spawn generation. | Same as above. |
| `ownership_adopted_only` | Only adopted sessions use the checkout, so the broker cannot prove it created it. | Same as above. |
| `ownership_reproof_refused` | The witness was saved before volume UUIDs and a remount renumbered the devices, and one re-proof condition failed. The reason names it. | Fix the named condition if it is cheap; otherwise `aethyme broker finish cleanup <session-id>`. |
| `ownership_unproven` | Another ownership proof failed. | Same as above. |
| `cohort_unavailable` | The closed checkout cohort could not be built. The reason carries the cohort code, for example `index_changed`. | Sweep again; if it persists, run `aethyme broker finish cleanup audit` and handle the checkout from its next action. |
| `checkout_missing` | The directory is gone and only the Git registration remains. | Review `git worktree prune --dry-run --verbose`, then prune. |
| `inventory_unresolved`, `removal_failed`, `other` | The reason says what failed. | Read `auto_cleanup.kept[].reason`, fix it, and sweep again. |
| `primary_checkout` | The path is the primary checkout. | Nothing to do; it is never removed. |

Automatic removal judges checkouts with a merged pull request or a due cleanup
request before it walks the rest of the lap, so a checkout that just became
removable does not wait for the lap to reach it. A removal that was interrupted
part-way is finished on a later pass.

If the count is still too high after the causes above are fixed, raising
`[worktrees].max_checkouts` buys time. Treat it as a stopgap: land the
configuration on the default branch and keep it finite.

## First Debug Commands

```bash
aethyme broker status doctor
aethyme broker unblock
cargo test --manifest-path packages/aethyme/rust/Cargo.toml --workspace
```
