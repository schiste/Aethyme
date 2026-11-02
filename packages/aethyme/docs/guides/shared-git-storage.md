# Shared Git storage maintenance

Last Updated: 2026-11-02

Worktree removal does not reclaim the repository's shared object database,
conflict-resolution cache, reflogs or worktree administration. A clean checkout
report is not a complete repository storage report.

`aethyme broker gc storage plan --json` includes `primary_checkouts[].git_storage`
even when source edits prevent deletion of primary build artifacts. Each entry
has a category, path, logical byte estimate (null means unknown), inode estimate,
measurement timestamp and maintenance guidance. These entries never become recursive deletion
candidates. Logical bytes are not APFS-exclusive physical allocation. Recorded
estimates can be old; an explicit measured plan spends at most 250 milliseconds
per repository on shared Git directories. Routine plans add these paths to the
existing bounded measurement rotation. A timeout reports unknown rather than a
partial total. Missing directories report zero.

## Non-pruning object maintenance

From an existing broker session in the owning repository:

```sh
aethyme broker advanced git --session SESSION --reason "Reviewed shared Git object maintenance" --queue-timeout 120 -- -c pack.threads=1 -c pack.windowMemory=32m -c gc.auto=0 maintenance run --task=loose-objects
```

This exact task is classified as a repository-wide write and uses the durable
operation journal and coordinator. It removes loose copies already present in
packs and packs a batch of remaining loose objects. It does not change branch
refs, expire reflogs, prune unreachable objects, remove worktrees or expire
conflict resolutions. Check disk headroom first: packing needs temporary space.
Run one pass at a time; compare `git count-objects -v` before and after. A pass
may leave substantial loose-object debt because Git bounds its batch. Do not
start a concurrent standalone `git gc`; it uses different Git maintenance locks.
Do not blindly retry an operation with unknown outcome; reconcile its receipt.

Other maintenance profiles, repacking, pruning and destructive rerere actions
are classified as destructive and require explicit authority. They are not an
automatic continuation of loose-object maintenance.

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
