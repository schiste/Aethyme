# Upgrading to Aethyme v0.7.25

Last Updated: 2026-09-22

## What is new

### Sessions can declare what they will work on

Leases are recomputed from a session's diff, so they describe work that has
already happened: by the time two sessions overlap on a path, both have edited
it. A session can now state its targets up front, and collisions between live
sessions are reported before any edit exists.

```
aethyme broker start --task "migrate callers off the old payment type" \
    --claim symbol:PaymentService=replace
```

```
Scope: 1 declared, 4 derived from the task
  high with session 9: one session rewrites `PaymentService` while the other extends it
    land the rewrite first, or extract the shared shape both sides can build on
```

`--claim` takes `kind:value[=operation]`. The operation is optional: a target
named without one records as `unknown` and reports at medium, rather than being
assumed safe. Severity is decided by the pair of intents — two sessions
extending one interface is ordinary work, while a rewrite underneath an
extension invalidates it — and is a pure function of the declarations, so the
same two claims always produce the same verdict.

Targets are also derived from the session task where the repository has graph
state. Derivation is best-effort: graph state is repository opt-in, and when it
is unavailable the reason is printed rather than leaving an empty scope set to
read as a clean bill of health. A declared target always beats a derived one.

Collisions appear in `aethyme broker status --json` as `scope_overlaps`,
alongside the existing path `overlaps`. A session's targets stop pairing
once it finishes, by the same liveness rule leases use, so the set describes
work in flight rather than accumulating every session that ever ran.

### A `none` contract decision can carry a stated justification

The cross-process contract check reads the diff, so it cannot tell a command
name leaving a comment from an entry point leaving the product. Where the
finding is a false positive, no truthful label existed: `soft-retire` and
`hard-delete` each assert a retirement that never happened.

A `none` decision is now accepted when the pull request body carries a
`Contract justification:` line of at least 24 characters. The finding is still
printed, with the justification beside it, so the hatch leaves evidence rather
than suppressing the check.

### Tracked symbols match on boundaries, not substrings

A tracked name inside a longer identifier no longer counts as a removal.

### Coordinated pushes refuse an ambiguous source and report what they sent

`broker git` runs inside the session worktree, so a `HEAD:` refspec resolved
there rather than where the operator stood — publishing that session's commit
under the branch name typed on the command line, and reporting success. A
`HEAD`-family push source is now refused when the caller is outside the session
worktree, and every successful push states what it sent:

```
operation 1476: git succeeded
  pushed 4cf586a3 -> refs/heads/my-branch
```

Refs under `refs/` are shared by every worktree, so branch names resolve
identically wherever the command runs and keep working.

### A submission that does not promote says why

A gate that refuses before spawning — disk headroom, a resource lease —
recorded `error` with a failure class and never ran the command. That reached
the caller as a bare "did not promote", indistinguishable from a gate that ran
and failed the change. The reason now travels with the outcome, naming the gate,
its class and its log.

### Integration reconcile no longer aborts on collected commits

A queue entry that never landed loses its commit to `git gc` eventually. That is
the expected end state, not corruption, but reconcile read its parents anyway
and aborted the whole pass, so post-merge cleanup deferred after every merge.
Terminal entries are now skipped whether or not their commit still exists; a
pending entry with a missing commit still fails loudly.

### Build provenance tracks the branch it was built from

`aethyme --version` reported the commit and date of an earlier build. The build
scripts watched `.git/HEAD`, which does not change when a branch advances — only
the ref it points at does. They now watch the ref and `packed-refs` as well, so
a pull, fast-forward or local commit produces honest provenance without a forced
rebuild.

## Compatibility

**Broker database schema 41 → 42.** This release adds the `session_scopes`
table. Migrations are append-only and there is no downgrade: once a database has
been opened by this release, an older `aethyme` binary cannot read it and will
refuse rather than guess.

Install the CLI and engine pair together, and before other agent sessions or
plugin hooks on the same machine next use the broker.

No command, flag or output was removed. `--claim` is new; `--scope` continues to
name the resource of a coordinated operation.

## Before upgrading

- Finish or close in-flight sessions, or be ready to reinstall before they next
  run a broker command.
- Confirm no other agent on this machine is mid-submission:
  `aethyme broker status --json`.

## Install or update

```
cargo install --locked --path packages/aethyme/rust/crates/aethyme-cli
cargo install --locked --path packages/aethyme/rust/crates/aethyme-engine
```

`--locked` is required, not stylistic: without it the resolver re-resolves
instead of using the workspace lockfile and selects a `ra-ap-rustc_lexer` that
fails to compile.

## Migrate and verify

The migration runs the first time the new binary opens the database.

```
aethyme --version          # build_commit should match the release tag
aethyme broker status      # opens the migrated database
```

A session started after upgrading reports its scope, and `status --json`
carries `scope_overlaps` once two live sessions name one target.

## Known issues

- Derived targets carry no operation, so a collision between two derived scopes
  reports at medium and says only that both sessions name the target. Declare
  the operation with `--claim kind:value=operation` where the intent is known.
- Derivation reads committed graph state. A repository without it records no
  derived targets and says so; explicit claims still work and still collide.
- Scope collisions are advisory. They are reported at session start and in
  `status --json`, and never refuse a session or block a submission.

## Rollback

The schema migration cannot be undone. To return to v0.7.24, reinstall that
version **and** restore a copy of `.aethyme/broker.db` taken before the upgrade;
an older binary will refuse a migrated database rather than operate on it.

Take that copy before upgrading if a rollback path matters to you.
