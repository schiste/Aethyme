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
resolution choices, compatibility decision, live session contracts, relevant
leases, exact dirty paths split into overlapping and disjoint sets, and the
local diff SHA-256. The plan digest binds every one of those fields. Task text,
absolute worktree paths, dirty file contents, and diff bodies are excluded from
JSON. Diff bodies also never enter broker reports, events, metrics, or command
telemetry.

Planning performs no writes to the selected repository and ignores dirty or
incidental worktree content when generating migration output. Dirty paths that
overlap an exact proposed write block application. Disjoint dirty paths may
remain: the plan warns that they were not inputs, and `apply` materializes only
the reviewed outputs from the disposable proposed tree. A live broker session
blocks any migration that changes shared policy or gate files until that
session finishes. An invalid marker, unsupported future schema, or
enrollment-mode mismatch also makes the plan unsafe. The remediation never
requires moving unrelated work out of the worktree.

Policy ownership is explicit. Aethyme updates only its marked blocks in
`.gitignore`, `AGENTS.md`, and `CLAUDE.md`; surrounding maintainer content is
preserved. `gates.toml` migrations are structural and preserve comments and
repository-defined gates. If a policy differs from every exact generated
version known to the binary and has no valid managed block, the plan is unsafe
until you provide a reviewed resolution file:

```json
{
  "schema_version": 1,
  "resolutions": {
    "AGENTS.md": "merge",
    "CLAUDE.md": "preserve",
    ".aethyme/gates.toml": "merge"
  }
}
```

`preserve` leaves the file byte-for-byte unchanged, `merge` retains
repository-owned content while adding or updating the Aethyme-owned structure,
and `replace` explicitly authorizes a complete generated replacement. Use the
same file for planning and application; its parsed choices are bound into the
plan digest:

```bash
aethyme upgrade plan --resolution-file /path/to/resolutions.json --diff
aethyme upgrade apply --resolution-file /path/to/resolutions.json \
  --confirm <plan-sha256>
```

A resolution for an unmanaged or non-customized path is rejected. Malformed
managed markers and unsupported gate schemas cannot be merged; choose
`preserve` or an explicit `replace` after review. Policy files are never
force-replaced merely because a binary is newer.

After reviewing the plan, committing any overlapping work, and finishing the
live sessions listed for a shared policy or gate migration, apply exactly that
plan. Unrelated dirty files may remain:

```bash
aethyme upgrade apply --confirm <plan-sha256>
git diff --check
git diff
aethyme deploy verify --repo .
```

`apply` refuses a shortened digest, overlapping dirty paths, changed session or
lease preconditions, or any source state that differs from the reviewed plan.
It writes only the exact reviewed output paths, verifies the resulting
deployment, and only then records the current repository schema. During
convergence the marker records an in-progress schema, so an interrupted process
cannot make an old or partial deployment appear current. If an upgrade is
interrupted, do not hand-edit the marker: inspect the diff, restore or commit
the intended state, obtain a fresh plan, and reapply.

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
