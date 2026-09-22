# Closed-session worktree cleanup

Last Updated: 2026-09-22

Removing a session worktree is a reviewed operation. A closed session is not
evidence that its work is safe to discard, and a worktree that looks clean can
still hold the only copy of something. The broker refuses by default and names
what it would need; this runbook is how an operator answers it.

The one rule underneath all of it: **decide by whether the content is
represented somewhere else, never by whether the directory looks idle.** Age,
size, and a clean `git status` are all compatible with unique work.

## 1. Inspect before planning

```sh
aethyme broker status --json
aethyme broker worktrees
```

`worktrees` is the loss report: every worktree on the host with its repository,
age, size, and what deleting it would cost — `recoverable`, `unpushed
(N commits)`, or `uncommitted (N files)`. Read the header first. It states how
many hold work that exists nowhere else, and no cleanup path can reclaim those.

A worktree outside this repository belongs to whoever ran it. Classify it, do
not act on it.

## 2. Dry-run the supported sweep

```sh
aethyme broker cleanup --all-cleaned
```

Read-only. It inventories every retained broker-owned worktree from an
already-closed session and prints, per session, a verdict and the two commands
that follow from it:

```
session 493: dirty (14.8 MiB) — worktree has uncommitted or untracked changes
  /…/worktrees/<root>/implement-210-merge-order-remediation-la
  refs/heads/agent/implement-210-… at 11000ba8925521c8fac8efbc3da81d64c4194bb7
  inspect: git show --stat --oneline 11000ba8925521c8fac8efbc3da81d64c4194bb7
  explicit discard: aethyme broker cleanup 493 --force
```

Verdicts and what each means:

| verdict | meaning |
| --- | --- |
| `recoverable` | clean, and its commits are represented on a delivery target |
| `dirty` | uncommitted or untracked changes in the worktree |
| `pending_commits` | commits after the adoption boundary that were never accepted |
| `unproven_provenance` | the session HEAD cannot be related to its recorded boundary, or the boundary is not on any delivery target |

`unproven_provenance` means the **SHA** is unreachable. It does not mean the
work is lost — a cherry-pick or a squash merge lands identical content under a
different SHA, and the broker cannot see that. Proving it is step 4.

Adopted worktrees are never in the bulk sweep. Neither are live sessions.

## 3. Apply the safe subset

```sh
aethyme broker cleanup --all-cleaned --apply --confirm <plan-sha256>
```

The digest comes from the plan you just read; a stale digest is refused rather
than reinterpreted. Apply revalidates before removing, so a worktree that went
dirty between plan and apply is skipped, not deleted on the older reading.

This removes only what it could prove. Everything else is left, and that is the
correct outcome — not a failure to reclaim.

## 4. Prove containment before discarding anything else

Everything remaining needs a per-worktree decision. Do not use a raw diff
against `main` to make it: once `main` has moved on, the diff is dominated by
`main`'s own later work and reports thousands of differences for a worktree that
contributed nothing.

Two tests actually discriminate.

**Tree identity** — strongest, when the content shipped under a different SHA:

```sh
tree=$(git -C <worktree> rev-parse HEAD^{tree})
git log --format='%T %h' origin/main | grep "^$tree "
```

A hit means some commit on `main` has a byte-identical tree. Deleting the
worktree loses nothing.

**Did `main` move, or did this worktree?** — for everything else:

```sh
base=$(git merge-base <worktree-head> origin/main)
git diff --name-only <worktree-head> origin/main | while read -r f; do
  [ -z "$(git log --oneline "$base"..origin/main -- "$f")" ] && echo "$f"
done
```

Every file this prints is one that differs and that `main` has **not** touched
since the base — the only candidates for unique work. An empty result means
every difference is `main` moving ahead.

For a `dirty` worktree, run the same test over `git status --porcelain` paths,
and read the uncommitted content: changes confined to generated artifacts
(`.aethyme/graph/`, `target/`) are not authored work.

## 5. Discard, with the reason recorded

```sh
aethyme broker cleanup <session-id> --force
```

`--force` is the operator asserting what the broker could not prove. Run it only
after step 4 produced evidence, and say what that evidence was — the assertion
is the point, and an unexplained `--force` is indistinguishable from a guess.

## 6. Retry and recovery

Cleanup is idempotent and needs no live agent. A removal interrupted partway is
judged and completed on the next run rather than refused, so the recovery for an
interrupted sweep is to run the sweep again.

If a plan digest is rejected, the state changed under you. Re-read the plan
rather than reaching for `--force`; the refusal is the mechanism working.

If `gc plan` reports bytes it cannot reclaim, check the blocker kind before
concluding the disk is unreclaimable:

- `retention_age` — inside `closed_worktrees_days`, held deliberately
- `unproven_contribution` — needs step 4
- `accepted_checkpoint` — another session names it as provenance

Two things `gc` does **not** see, which have both cost real time: the gate cache
under the OS cache directory (#295), and worktree roots with no marker file
(#257). A reclaimable-bytes figure near zero is a statement about what `gc`
measures, not about the disk.

## What is protected, and stays protected

Unpushed commits, tracked edits, untracked source files, active leases, the
current worktree, and unverified promotion provenance all block automatic
removal. Every one of those is reachable only through an explicit, per-session
`--force`. The fail-closed property is the feature: when the broker cannot
establish ownership, cleanliness, provenance, or liveness, it retains the
worktree and states the uncertainty rather than guessing.

## Related

- [`operational-recovery.md`](operational-recovery.md) — installation,
  configuration and integration ancestry
- [`host-resource-coordination.md`](host-resource-coordination.md) — host-wide
  capacity and leases
