# Shared Git storage maintenance

Last Updated: 2026-11-02

Worktree removal does not reclaim the repository's shared object database,
conflict-resolution cache, reflogs or worktree administration. A clean checkout
report is not a complete repository storage report.

`aethyme broker gc storage plan --json` includes `primary_checkouts[].git_storage`
even when source edits prevent deletion of primary build artifacts. Each entry
has a category, path, logical byte estimate (null means unknown), inode estimate,
measurement timestamp, blockers and maintenance guidance. These entries never become recursive deletion
candidates. Logical bytes are not APFS-exclusive physical allocation. Recorded
estimates can be old; an explicit measured plan spends at most 250 milliseconds
per repository on shared Git directories. Routine plans add these paths to the
existing bounded measurement rotation. A timeout reports unknown rather than a
partial total. Missing directories report zero. Symlinked or inaccessible Git
categories stay unknown; measurement never follows them.

## Non-pruning object maintenance

From an existing broker session in the owning repository:

```sh
aethyme broker advanced git --session SESSION --reason "Reviewed shared Git object maintenance" --queue-timeout 120 -- -c pack.threads=1 -c pack.windowMemory=32m -c gc.auto=0 maintenance run --no-quiet --task=loose-objects
```

Repository-routing overrides are refused for coordinated maintenance.
This exact task is classified as a repository-wide write and uses the durable
operation journal and coordinator. It removes loose copies already present in
packs and packs a batch of remaining loose objects. It does not change branch
refs, expire reflogs, prune unreachable objects, remove worktrees or expire
conflict resolutions. Check disk headroom first: packing needs temporary space.
Run one pass at a time; compare `git count-objects -v` before and after.
An existing `objects/maintenance.lock` is reported as a blocker. Git may skip
maintenance and return success while that lock exists; exit status alone is
not proof of reclamation. Do not remove a lock from age alone. A pass
may leave substantial loose-object debt because Git bounds its batch. Do not
start a concurrent standalone `git gc`; it uses different Git maintenance locks.
Do not blindly retry an operation with unknown outcome; reconcile its receipt.

Other maintenance profiles, `git gc`, repacking, pruning and destructive rerere actions
are classified as destructive and require explicit authority. They are not an
automatic continuation of loose-object maintenance.

## Recovering a lock left before this OS boot

```sh
aethyme broker gc storage maintenance-lock plan --json
aethyme broker gc storage maintenance-lock apply --confirm REVIEWED_SHA256 --json
```

This command targets only the exact `objects/maintenance.lock` in the invoking
repository's canonical shared Git directory. It never recurses through Git data.
A plan is eligible only for an empty, regular, single-link file owned by the
current user whose available timestamps all predate the current OS boot by at
least 60 seconds. Age alone, an unavailable boot time, or uncertain file identity
cannot authorize recovery. Repository-routing overrides are refused.

Apply revalidates the digest-bound repository, directories, file and boot proof
under the broker's repository-wide write lane. Pending or outcome-unknown Git
writes block recovery across every remote key. A durable intent precedes unlink;
anchored directory operations recheck file identity, and a durable receipt follows
directory synchronization. Branches, objects, index, source and saved resolutions
remain unchanged. A refusal or incomplete receipt returns a nonzero exit status.

Review both the structured result and the preserved intent/receipt under
`.aethyme/git-maintenance-recovery/`. If an intent already exists, recovery refuses
rather than retrying that digest. Inspect the target and records before any next
step; do not delete recovery records to force a retry. A lock created during this
boot stays protected even if its creating process appears absent. This deliberately
narrow workflow does not resolve every possible stale Git lock.

After successful recovery, run the coordinated non-pruning maintenance task above
one bounded pass at a time. Recovery itself does not reclaim object storage.

## Conflict reuse and recovery history

`rr-cache` contains conflict preimages and saved resolutions, sometimes many
large variants of the same file. Expiring a worktree does not expire that cache.
Git's normal rerere expiry keeps resolved records for 60 days and unresolved
records for 15 days. The checkout grace period is a different policy.

Do not remove `rr-cache` wholesale or run accelerated rerere expiry just to hit
a storage target. Before any expiry, account for merge/rebase state and
`MERGE_RR` in every linked worktree, coordinate writers, and retain records whose
use is uncertain. This inventory does not authorize conflict-cache expiry.
Reflogs, broker coordination data and recovery refs remain retained as well.

References: [Git maintenance](https://git-scm.com/docs/git-maintenance),
[Git rerere](https://git-scm.com/docs/git-rerere).
