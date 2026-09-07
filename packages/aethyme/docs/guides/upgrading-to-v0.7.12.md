# Upgrading to Aethyme v0.7.12

Last Updated: 2026-09-07

v0.7.12 adds three fleet-scale practices to the generated agent policy and
answers whether sandboxed execution is a supported way to participate in
coordination. No schema changes.

## Compatibility

| Contract | v0.7.12 |
| --- | --- |
| Supported platforms | Apple Silicon macOS, Intel macOS, x86-64 Linux |
| Required executables | `aethyme`, `aethyme-engine-cli` from the same build |
| Minimum Git | 2.38 |
| Engine daemon protocol | 1 |
| Broker storage | reads/migrates schemas 1 through 31; writes schema 31 |
| Repository deployment | schema 1; no mandatory migration from v0.7.11 |
| Graph cache schema | 1; derived and safe to remove |
| Release channel | `stable` |

The signed release manifest binds these contracts and every archive digest to
the exact source SHA.

## What changed

### The generated policy gains three practices

Each ships with its reason attached, because a rule an agent cannot justify is
one it will reflexively break:

- **No catch-up merges.** Do not merge the default branch into a working branch
  to keep it current. Merge or rebase only to resolve an actual conflict, and
  only in your own worktree. Where the default branch does enforce
  branch-freshness, update immediately before merging rather than throughout the
  branch's life.
- **Required checks proportional to the diff.** A check may be required on a
  pull request only if it is scoped to the diff and finishes in minutes.
  Anything heavier runs once per merge batch or once per release. With one agent
  an unscoped required check is an annoyance; with several it serialises the
  fleet.
- **Cherry-pick to patch, not to assemble.** Patching an already-certified
  release is standard. Assembling a release from trunk produces a combination
  nobody tested and hides dependencies between the picks.

### Sandboxed execution is supported, on stated terms

Coordination state is host-scoped because it coordinates across worktrees and
repositories on one machine. A sandbox confined to a single checkout cannot open
it, and the two remedies are not equivalent:

- **Participating** — grant the process access to the host state path. It
  coordinates with every other session on the machine.
- **Isolating** — point `AETHYME_HOST_STATE_DIR` at a writable location. This
  always succeeds and yields a **private coordination domain**: it sees no other
  session's leases, sessions or host resources, and none see its.

The permission error now says which one you are choosing. There is no third
option in which a confined process participates in host-wide coordination
without host-wide state, because the shared state is the coordination.

## Before upgrading

No schema migration runs, and mixed v0.7.11/v0.7.12 installations can share a
repository during a rollout.

**The generated policy changes.** The next `aethyme enhance deploy` in an
enrolled repository rewrites `AGENTS.md` and `CLAUDE.md` with the new section,
and the `policy-sha256` stamp changes with it. If a repository has hand-edited
either file, deploy refuses rather than overwriting and directs you to
`aethyme upgrade plan --repo . --diff`; move the customization into
`.aethyme/overrides/agents.json` and redeploy.

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
aethyme enhance deploy --repo .
aethyme enhance verify --repo .
aethyme broker quick-test
git diff --stat AGENTS.md CLAUDE.md
```

Both version commands must report `0.7.12`. The diff should show the new
"Branch And Review Practices" section and an updated `policy-sha256`; review and
commit it with your repository's normal policy.

## Rollback

Restore both v0.7.11 binaries together through the original installation
manager. No schema migration has run, so no database restore is required.
Redeploy to regenerate the previous policy text, or revert the committed
`AGENTS.md` and `CLAUDE.md`. Never combine binaries from different releases.

## Known issues

- Graph support remains opt-in. Cold refresh and one-file refresh are still
  expensive on very large repositories; Aethyme itself remains graph-disabled.
- A large graph may use hundreds of MiB for committed fragments, each private
  redb, and the optional host cache. Cache retention is not yet automatic.
- The no-catch-up-merge rule ships as a static practice. Emitting it
  conditionally on the default branch's freshness requirement would need a
  provider API call inside `enhance deploy`, which is offline today.
- Coordinated operations still serialize per repository.
  `[coordination] hooks_outside_lock` removes the local gate from the critical
  section, but two operations on unrelated refs still contend.
- `main reconcile` classifies commits as represented or not; four-way
  classification with resolution templates is not implemented.
- Schema migration, when a release carries one, is implicit. This release
  carries none.
- A dead holder's namespace and exclusive-key allocations still require
  `aethyme broker resources reconcile <lease-id> --confirm <generation>`.
- Windows and Linux ARM archives are not published.
- Update checks are explicit; Aethyme performs no background network request.
- Homebrew installations must be upgraded through Homebrew.
