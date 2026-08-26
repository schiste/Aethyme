# Repository upgrades

Last Updated: 2026-08-26

Aethyme has two separate upgrade boundaries:

1. Homebrew or the paired installer updates `aethyme` and
   `aethyme-engine-cli` for the machine.
2. `aethyme upgrade` updates Aethyme-owned policy, configuration, hooks, and
   generated guidance for one explicitly selected repository.

The binary updater never searches the laptop for repositories and never
changes a repository implicitly. After a binary update, enter each enrolled
repository and create a read-only plan:

```bash
cd /path/to/repository
aethyme upgrade plan --diff
```

The local diff is generated only from the exact committed HEAD in a disposable
repository. It includes creates, updates, deletes, file-mode changes, and
binary patches without reading incidental ignored or untracked files. For
automation or durable review metadata, request the content-free plan instead:

```bash
aethyme upgrade plan --json
```

The plan identifies the current and target repository schema, exact embedded
migrations, repository-relative paths that may change, the source Git HEAD,
existing managed-state digest, proposed content hashes and modes, unresolved
resolution choices, compatibility decision, active session contracts, and the
local diff SHA-256. The plan digest binds every one of those fields. Task text,
absolute worktree paths, and diff bodies are excluded from JSON. Diff bodies
also never enter broker reports, events, metrics, or command telemetry.

Planning performs no writes to the selected repository and ignores dirty or
incidental worktree content when generating migration output. Applying still
requires a clean worktree. An invalid marker, unsupported future schema, or
enrollment-mode mismatch makes the plan unsafe.

After reviewing the plan and ensuring the repository is clean, apply exactly
that plan:

```bash
aethyme upgrade apply --confirm <plan-sha256>
git diff --check
git diff
aethyme deploy verify --repo .
```

`apply` refuses a shortened digest or any repository state that differs from
the reviewed plan. It converges the repository from templates and migration
logic embedded in the installed binary, verifies the resulting deployment,
and only then records the current repository schema. During convergence the
marker records an in-progress schema, so an interrupted process cannot make an
old or partial deployment appear current. If an upgrade is interrupted, do
not hand-edit the marker: inspect the diff, restore or commit the intended
state, obtain a fresh plan, and reapply.

## Canonical deployment

Canonical repositories track `.aethyme/repository.json`. That marker follows
normal Git history, so another clone immediately knows which repository
contract it received. Migration output is intentionally left as a normal
working-tree diff for maintainer review and commit. Until the marker and its
associated files land together, other clones remain on the prior contract.

The first `aethyme broker ...` invocation in an enrolled repository refuses
to run when the marker is missing, incomplete, or newer than the installed
binary. This prevents agents from operating under stale generated rules. It
does not affect a repository that has not been enrolled with Aethyme.

## Local-only deployment

For a locally activated bridge, use the matching mode:

```bash
aethyme upgrade plan --local-only
aethyme upgrade apply --local-only --confirm <plan-sha256>
```

The marker lives at `.aethyme/local/repository.json` and remains ignored with
the rest of local-only deployment state. The migration affects only that
worktree; collaborators and fresh clones remain inactive until they opt in.

## Release and rollback contract

The signed release manifest advertises the repository schema supported by the
binary in addition to engine protocol and broker-storage compatibility. Every
release that changes repository-owned files must ship the corresponding
embedded migration and describe its downgrade behavior in the release notes.

Rolling back the binary pair does not automatically reverse a committed
repository migration. For a canonical deployment, revert the migration commit
with normal Git before using a binary that does not support the newer schema.
For local-only deployment, restore a clean local state and redeploy with the
selected older binary. Never restore only one executable from the binary pair.
