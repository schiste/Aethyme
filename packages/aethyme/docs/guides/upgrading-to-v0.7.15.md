# Upgrading to Aethyme v0.7.15

Last Updated: 2026-09-08

v0.7.15 routes pull request review activity back to the broker session that
opened the pull request. No schema changes, and no change to the generated
agent policy.

## What is new

Monitoring is **off until a session asks for it**, because a fleet that watched
every pull request it opens would make provider calls nobody requested and
interrupt agents that never opted in.

```bash
aethyme broker watch pr monitoring activate --session <id>
```

While active, a pull request opened through `broker gh` starts its watch
automatically. The watch covers comments and reviews, not checks, so check
churn cannot bury the review it exists to deliver.

Delivery resolves the Chau7 tab running a session by **worktree identity**, not
by branch: broker worktrees are unique per session, whereas several agents
routinely share one branch. Ambiguity refuses rather than guessing, because a
missed notification is recoverable and one delivered into an unrelated agent's
terminal is not.

The broker still starts no background poller. A host scheduler drives one
bounded `watch pr tick`, and the transport lives outside the broker entirely:

```bash
aethyme broker watch pr tick --limit 20
python3 packages/aethyme/scripts/adapters/chau7-delivery-adapter.py --worker <id>
```

`packages/aethyme/scripts/adapters/` carries a launchd agent template that runs
both every five minutes.

## Compatibility

No schema change, no change to the generated agent policy, and no command
removed or renamed. A repository enhanced at an earlier 0.7.x needs no
redeployment to keep working; redeploy only to pick up the refreshed policy
stamp.

The new pull request monitoring is inert until a session opts in, so upgrading
alone changes no behaviour.

## Before upgrading

Nothing is required. If you intend to use the scheduled monitoring, note that
the transport adapter needs Python 3 and a running Chau7; without either, the
broker is unaffected and simply delivers nothing.

## Install or update

The router and its engine sibling are one release unit -- never install one
without the other.

```bash
cargo install --path packages/aethyme/rust/crates/aethyme-cli --force
cargo install --path packages/aethyme/rust/crates/aethyme-engine --force
# or, from the tap
brew update && brew upgrade aethyme
```

## Migrate and verify

There is nothing to migrate. Confirm both halves report the new version, then
confirm the broker still reads its existing state:

```bash
aethyme --version
aethyme broker status
aethyme broker watch pr monitoring status --session <id>
```

If `aethyme` still reports an older version, check for a second copy earlier on
`PATH` -- a tap install and a `cargo install` can shadow each other.

## Rollback

Reinstall the previous pair. No state written by 0.7.15 is unreadable by
0.7.14: monitoring activation is a marker file under `.aethyme/run/`, which an
older binary ignores, and the delivery backoff is computed from columns that
already existed.

```bash
brew install schiste/tap/aethyme@0.7.14   # or reinstall the previous archives
```

Delete `.aethyme/run/pr-monitoring/` if you want the opt-in state gone too.

## Known issues

- A delivery whose target tab has closed defers indefinitely rather than
  failing. It no longer blocks other deliveries, but it is never retired.
- A session whose work lands through a provider-side squash merge cannot be
  closed, because representation is decided against broker promotions rather
  than the default branch.

## Fixed

- A deferred delivery is no longer re-claimed immediately. It previously
  re-entered the front of the queue and starved every delivery behind it for
  that adapter, so one busy agent could silently block notifications to all the
  others.
- Checkout discovery no longer resolves to an enclosing repository when a gate's
  merge-simulation worktree has been removed.
