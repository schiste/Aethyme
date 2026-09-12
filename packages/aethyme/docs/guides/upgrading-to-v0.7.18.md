# Upgrading to Aethyme v0.7.18

Last Updated: 2026-09-12

v0.7.18 is the review-routing release. A repository can now decide which
reviews a change needs, hand each one to an agent in its own throwaway
checkout, show the state on the pull request, and close the row when the
reviewer reports. Every part of it is off by default.

It also carries the fix for a `git` wrapper that made every clean worktree on
a machine read dirty, which had been silently disabling cleanup.

This release migrates the broker database from schema 32 to 35. Read
**Rollback** before upgrading.

## What is new

### Review routing

Three opt-in tables in `.aethyme/config.toml` decide the whole thing:

- `[review.trigger]` — which reviews a change needs.
- `[review.routing]` — who performs each one, and how many may run at once.
- `[review.projection]` — what the pull request shows: one Aethyme-owned
  comment and namespaced labels.

Every table is off by default at every level. A repository that has not opted
in gets pull requests byte-for-byte identical to before.

The commands:

```bash
aethyme broker review plan --pr 180                    # decide, perform nothing
aethyme broker review run --session <id> --repo <o/n> --pr 180 --from-provider
aethyme broker review tick --session <id> --repo <o/n> # sweep every open PR
aethyme broker review ledger --repo <o/n> [--pr 180]   # what was asked, and how it ended
aethyme broker review state --repo <o/n> --pr 180 --type code --state satisfied
```

The split to understand: **the broker decides; the caller performs the
transport.** `review run` picks the review, the workspace, and the prompt, and
records the row *before* anything is handed out. It never speaks to an agent
runner, so it builds and tests with none present.
`packages/aethyme/scripts/adapters/chau7-review-adapter.py` performs the other
half — checkout, spawn, tab teardown.

That boundary has a consequence worth knowing before you run these by hand:
`review run` writes the ledger row and *prints* the handoff. If you run it
yourself and do nothing with the printed handoff, the row still reads
`requested` and the router will not dispatch that dimension again until the
route's `stale_after_minutes` expires. Drive the loop through the adapter, or
release a stranded row with `review state ... --state abandoned`, which is the
one revivable state.

### An abbreviated head is now accepted

`review ledger` prints twelve characters of the head. `review state --head`
used to require all forty, so the one spelling in front of an operator was the
one spelling the command refused — and its refusal pointed back at the ledger.

A short head now resolves against the heads recorded for that pull request and
dimension, the way `git` resolves a short commit. Two candidates under one
prefix are refused by name rather than settled by recency: the reporter cannot
say which review it performed, and taking the newer one would file a verdict
against a commit nobody read.

### A reviewer's tab and row are now closed when it finishes

A reviewer's shell is interactive and never exits on its own. Until this
release nothing closed one, so a finished review held its workspace until
`stale_after_minutes` reclaimed the row as `abandoned` — filing a review that
ran and posted as one that never happened.

Teardown now runs before dispatch in each tick, because a tick may both reclaim
a dimension's workspace and dispatch a new review into it.

Note the second meaning this gives `stale_after_minutes`: it used to buy back
only a concurrency slot, and now it force-closes a live reviewer's tab. It is a
kill timer. The default is six hours; a value tuned when it only freed a slot
is probably too short now.

## Compatibility

**The broker database migrates from schema 32 to 35** — `review_requests`
(v33), its rebuild with the uniqueness constraint that makes an interrupted
`review run` re-runnable (v34), and `pull_request_observations` (v35).

The migration runs automatically the first time a v0.7.18 binary opens the
database, and it is one-way. The broker database is machine-wide, so **the
moment any v0.7.18 binary opens it, every older `aethyme` on the machine is
locked out** with a schema-too-new error — including the copies the agent
plugin's hooks invoke. Upgrade every copy on the machine, not just the first
one `PATH` resolves.

Nothing else changes behaviour. With the review tables absent or disabled, the
router performs no GitHub writes, spawns nothing, and leaves pull requests
untouched.

## Before upgrading

Account for every copy of the pair on this machine. A tap install and a
`cargo install` shadow each other, and only the one earliest on `PATH` runs:

```bash
which -a aethyme aethyme-engine-cli
```

Back up the broker database, because the schema migration is one-way and this
is the only way back to 32:

```bash
cp .aethyme/broker.db .aethyme/broker.db.pre-0.7.18
```

Finish or record any in-flight sessions first:

```bash
aethyme broker status
```

## Install or update

The router and its engine sibling are one release unit — never install one
without the other.

```bash
brew update && brew upgrade aethyme
# or, from a checkout
cargo install --path packages/aethyme/rust/crates/aethyme-cli --force
cargo install --path packages/aethyme/rust/crates/aethyme-engine --force
```

If a `git` wrapper on this machine decorates command output, install from a
`PATH` with it stripped. The broker now probes for an honest `git` and repairs
itself, but `cargo` is shadowed by the same class of wrapper and the build is
not self-healing.

## Migrate and verify

Migration happens on first open. Confirm the version and that the database
came up:

```bash
aethyme --version
aethyme certify
aethyme broker status
```

Review routing stays inert until you opt in. To see what it would do without
performing anything:

```bash
aethyme broker review plan --pr <number>
```

That command takes no session and writes nothing. It names both the tree it
read policy from and the tree it read the change from — policy comes from the
main checkout, never from the branch under review, so a pull request cannot
alter the rules that judge it.

## Rollback

**Constrained.** The schema migration is one-way: a v0.7.17 binary cannot open
a database at schema 35, and there is no down-migration. Rolling back the
binary alone leaves every `aethyme` command failing against the machine's
broker state.

To roll back, restore the database alongside the binary:

```bash
aethyme broker status                      # note anything in flight
cp .aethyme/broker.db.pre-0.7.18 .aethyme/broker.db
brew install schiste/tap/aethyme@0.7.17    # or cargo install --version 0.7.17
```

Sessions, operations, and review rows recorded under 0.7.18 are lost in that
restore. If you have no backup, the remaining option is to stay on 0.7.18 for
the broker and pin the older binary out of `PATH`.

The agent plugin can stay installed across a rollback; only the review
subcommands disappear.

## Known issues

`review run` and `review tick` shell out to read-only `gh`. A pull request that
`gh` cannot read is recorded in the sweep report and skipped, never fatal — but
an unauthenticated `gh` makes every pull request fail that way, which reads as
"nothing to review" rather than as an error.

The review workspaces under `.aethyme/reviews/pr-<n>/<dimension>/` are
registered git worktrees, and nothing reclaims them on a schedule. A dimension's
workspace is reused by the *next* review of the same pull request and dimension,
so a closed pull request's workspaces persist until removed by hand with
`git worktree remove`. Neither `aethyme broker cleanup` nor `aethyme broker gc`
reclaims them: both act on session worktrees, and a review workspace is not one.
They are invisible to `broker status` and to the retained-bytes budget, and a
single pull request's pair measured 5.6 GB on this project.

`stale_after_minutes` now force-closes a live reviewer's tab rather than only
freeing a slot. Re-tune any value that was chosen under the old meaning; too
short now kills a working reviewer mid-thought and files its dimension as never
answered, which nothing detects.

Teardown does not close a reviewer that changed directory. `finished_workspaces`
matches a tab to its workspace by comparing `tab.cwd` to the workspace path as
exact strings, and an interactive reviewer that runs `cd packages/...` — as an
agent reading a repository ordinarily will — moves its `cwd` out of the
workspace for the rest of its life. The tab is then never matched, so it is
never closed, and nothing else closes an interactive shell: it leaks until the
operator closes it. Worse, the same comparison backs the dispatch-side
occupancy check, so the workspace reads as free and the next review of that
dimension spawns into a directory another shell still occupies.

The observable symptom is `closed=0` from the adapter, which is exactly what
"nothing needed closing" looks like. Until this is fixed, check for orphaned
reviewer tabs by hand after a review completes rather than trusting the count.
