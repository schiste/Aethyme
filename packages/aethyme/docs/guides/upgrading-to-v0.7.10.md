# Upgrading to Aethyme v0.7.10

Last Updated: 2026-09-06

v0.7.10 is a correctness and diagnosability release for the broker's reviewed
operations. It repairs a dispatch defect that let one command's digest be
accepted by another, and makes refusals across the plan/apply family say what to
do next. No schema changes.

## Compatibility

| Contract | v0.7.10 |
| --- | --- |
| Supported platforms | Apple Silicon macOS, Intel macOS, x86-64 Linux |
| Required executables | `aethyme`, `aethyme-engine-cli` from the same build |
| Minimum Git | 2.38 |
| Engine daemon protocol | 1 |
| Broker storage | reads/migrates schemas 1 through 31; writes schema 31 |
| Repository deployment | schema 1; no mandatory migration from v0.7.9 |
| Graph cache schema | 1; derived and safe to remove |
| Release channel | `stable` |

The signed release manifest binds these contracts and every archive digest to
the exact source SHA.

## What changed

### A digest could be accepted by the wrong command

Readiness remediation matched on the verb alone, so `broker <group> plan`,
`apply` and `recover` routed to it for **every** group. `gc plan`, `ship plan`
and `promotion-record plan` each returned a readiness remediation plan and its
digest, and `checkpoint plan --session <id>` failed as an unknown option.

The digest is the dangerous part: `gc apply --confirm <digest>` would have
accepted the readiness digest and applied something never reviewed. Each command
now reaches its own implementation.

**If you recorded any plan digest from v0.7.8 or v0.7.9, discard it and
re-review.** A digest captured then may not describe the command you ran.

### Refusals name what to do next

Confirmation mismatches no longer print the expected digest as a value to paste,
which would confirm a plan nobody read. They name the command that re-reviews
instead. `gc apply` additionally distinguishes a stale confirmation from a
pending interrupted run, because those need opposite actions. `ship` keeps both
SHAs, which are inspectable and are the artifact under review, and warns that
confirming a moved prefix publishes unreviewed work.

### Publication states what it does not cover

`Freshness: Ready` describes the exact prefix, not the repository. `ship plan`
now says so:

```
Repository-wide publication: INCOMPLETE
  excluded: local refs/heads/main carries commits this prefix does not contain;
            list them with `git log --oneline <publication>..<local>`
  excluded: 2 uncommitted tracked path(s): a.rs, b.rs
```

Plans also report the entries this push newly publishes rather than the whole
included prefix, and a refused `--sync-main` names a non-destructive recovery
for every cause.

### State tracking across sessions

`broker status` surfaces commits the local default branch carries that
integration does not, so writes that bypassed submit are visible immediately.
`broker adopt` reports drift for every adoption and says when pre-existing
commits are not session-owned; `broker submit` names them rather than reporting
that nothing remains.

## Before upgrading

Nothing special. Broker storage, repository deployment, engine protocol and
graph cache schemas are unchanged from v0.7.9, so no migration runs and mixed
v0.7.9/v0.7.10 installations can share a repository during a rollout.

Discard any plan digests recorded under v0.7.8 or v0.7.9, as described above.

## Install or update

```bash
brew update
brew upgrade aethyme
```

Installer-managed users should review and confirm the signed manifest plan:

```bash
aethyme update check
aethyme update plan --channel stable
aethyme update execute --confirm <manifest-sha256>
```

## Migrate and verify

```bash
aethyme --version
aethyme-engine-cli --version
aethyme deploy --repo .
aethyme enhance verify --repo .
aethyme broker quick-test
aethyme broker gc plan
```

Both version commands must report `0.7.10`. `gc plan` must print a GC plan
rather than a readiness remediation plan -- that is the dispatch fix, and the
quickest way to confirm you are running a build that has it.

## Rollback

Restore both v0.7.9 binaries together through the original installation
manager. No schema migration has run, so no database restore is required and
repository deployment files are compatible in both directions. Never combine
binaries from different Aethyme releases.

## Known issues

- Graph support remains opt-in. Cold refresh and one-file refresh are still
  expensive on very large repositories; Aethyme itself remains graph-disabled.
- A large graph may use hundreds of MiB for committed fragments, each private
  redb, and the optional host cache. Cache retention is not yet automatic.
- Schema migration, when a release does carry one, is implicit: the first newer
  binary to open the database migrates it, with no confirmation and no warning
  to other installations. This release carries none.
- There is no reviewed flow for absorbing local default-branch work that never
  entered the broker; `ship` reports the divergence and refuses to discard it.
- A session whose target paths were renamed by a later promotion is not told so
  before a replay fails.
- Coordinated operations serialize per repository, and the lock is held across
  the wrapped command's local hooks; use `--queue-timeout` or `--no-wait` to
  avoid parking behind a slow gate.
- A dead holder's namespace and exclusive-key allocations still require
  `aethyme broker resources reconcile <lease-id> --confirm <generation>`; only
  capacity units are reclaimed automatically.
- Windows and Linux ARM archives are not published.
- Update checks are explicit; Aethyme performs no background network request.
- Homebrew installations must be upgraded through Homebrew.
