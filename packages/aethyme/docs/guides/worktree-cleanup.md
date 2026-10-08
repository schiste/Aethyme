# Closed-session worktree cleanup

Last Updated: 2026-10-08

Removing a session worktree is a reviewed operation, with one deterministic
exception described under *Automatic removal* below. A closed session is not
evidence that its work is safe to discard, and a worktree that looks clean can
still hold the only copy of something. The broker refuses by default and names
what it would need; this runbook is how an operator answers it.

The one rule underneath all of it: **decide by whether the content is
represented somewhere else, never by whether the directory looks idle.** Age,
size, and a clean `git status` are all compatible with unique work.

## Lifecycle and proof model

A session, its worktree, and its branch have separate lifecycles. Finishing a
session closes broker ownership; it does not by itself authorize deleting either
the checkout or its branch. Delivery changes the target refs, while cleanup
proves the session's exact content against the fetched delivery targets. The
proof accepts ancestry for merge and fast-forward delivery and content or patch
identity for squash and rebase delivery. If it cannot decide, it retains the
work and reports the blocker.

`finish`, the cleanup audit and cleanup plan use that proof to explain whether
a session is represented. Automatic worktree cleanup and reviewed GC apply use
the same worktree eligibility checks, then revalidate ownership, liveness,
cleanliness and the expected branch tip immediately before removal. `ship` publishes a reviewed
integration entry; integration reconciliation updates that entry from the
fetched upstream and pending queue. Neither operation alone authorizes
deletion: cleanup checks the resulting delivery target again. Host storage
planning adds a read-only view of roots, caches and broker branch refs; it
reports unknown ownership for review rather than turning it into a deletion
candidate.

## 1. Inspect before planning

```sh
aethyme broker status --json
aethyme broker advanced worktrees
```

`worktrees` is the loss report: every worktree on the host with its repository,
age, size, and what deleting it would cost — `recoverable`, `unpushed
(N commits)`, or `uncommitted (N files)`. Read the header first. It states how
many hold work that exists nowhere else, and no cleanup path can reclaim those.
Sizes come from recorded measurements and a 10 s walk budget; a `?` size was
not measured in time and the header says the totals are floors. Add
`--measure` when you need every size. Discovery follows Git checkout metadata,
not broker root markers, so markerless checkouts remain visible with their
recoverability evidence.

A worktree outside this repository belongs to whoever ran it. Classify it, do
not act on it.

For a host-level view of worktree roots, preparation-cache entries, primary
checkout artifacts, and broker branch refs, use:

```sh
aethyme broker gc storage plan --json
```

Each owned root's `session_branches` lists local refs recorded by its session
ledger and unclaimed refs in the broker's `agent/` namespace. Entries include
the head SHA, session IDs, uncleared holders, registered checkouts, and a reason
for retention. `session_branch_inventory_complete: false` means the owner or
its refs could not be fully inspected; an empty list in that case does not
mean there are no branches. An unclaimed `agent/` ref has unknown ownership,
so the plan reports it for review and never offers it as an automatic removal
candidate. Branch deletion still needs independent ownership and delivery
proof. The storage plan is read-only with respect to session and branch state;
its digest authorizes only the listed filesystem candidates.

This output is schema version `3`. The summary reports the counts of
ledger-owned branch refs, unclaimed agent refs, and valid roots whose branch
inventory is incomplete. These counts describe refs visible in each inspected
storage root; they are not deletion candidates or proof of delivery.

For one repository, the audit answers the same question by content and against
a named target:

```sh
aethyme broker finish cleanup audit            # or --repo <path>, --detail, --json
```

It states the exact commit it proved against (the fetched upstream, not a local
`main` that trails it), and separates `in_target`, `integration_only`,
`remote_branch_only` (pushed, e.g. an open pull request), `worktree_only`,
`dirty`, `live`, `missing_checkout_metadata` and `unknown_provenance`, each with
a blocker and a next action. Squash and rebase landings count as `in_target`;
when the cleanup plan still calls one unproven, the next action is
`aethyme broker advanced representation scan` and then `record`, not `--force`.
The proof is ordered by cost (#588). A head that is an ancestor of any
delivery target is landed by `ancestry`, checked against every target before
any target gets the content and patch search, so a merge or fast-forward
delivery never pays a candidate walk. Only squash and rebase deliveries, which
ancestry cannot see, reach that search, and its patch comparison is verbatim
(#586). Measured on a branch merged by a merge commit, with a diverged target
of 2,000 commits listed first: 381 ms before, 10 ms after.
Registrations whose directory is gone are administrative metadata: `git
worktree prune --dry-run --verbose` handles them, separately from removing a
checkout or a branch.

## Automatic removal (#588)

The unattended sweep removes a closed checkout without a reviewed plan only
when four proofs hold, each re-checked under the GC lock immediately before
the directory goes:

1. **Sessions** -- every broker session that names the worktree is closed.
2. **No use** -- no process has a file or working directory open in it, no
   lease that still holds covers it, and no gate is running.
3. **Clean** -- no tracked, staged or untracked change, no stash made on its
   branch, no rebase, merge, cherry-pick, revert or bisect in progress, and no
   ignored path outside the regenerable set (`target/`, `node_modules/`,
   `.venv/`, `build/`, `dist/`, `.DS_Store`, plus `[cleanup] regenerable`).
4. **Contained** -- every commit is in the fetched remote default branch, by
   ancestry or by the verbatim content/patch proof. Local `main` and the
   integration branch do not count.

Anything the sweep cannot decide keeps the checkout, with the reason in
`aethyme broker gc plan --json` (`auto_cleanup.kept[]`) and a
`cleanup.auto-removal` row in `broker status`. Each removal emits
`broker.cleanup.auto_removed` with its proof. Configure it in
`.aethyme/config.toml`:

```toml
[cleanup]
auto_remove = true             # the default; false turns it off
regenerable = [".gradle/**"]   # extra rebuildable paths
keep = ["*-investigation"]     # worktrees never removed automatically
```

Build caches of inactive sessions are still reclaimed separately, even when a
checkout must be kept.

## 2. Dry-run the supported sweep

```sh
aethyme broker finish cleanup --all-cleaned
```

Read-only. It inventories every retained broker-owned worktree from an
already-closed session and prints, per session, a verdict and the two commands
that follow from it:

```
session 493: dirty (14.8 MiB) — worktree has uncommitted or untracked changes
  /…/worktrees/<root>/implement-210-merge-order-remediation-la
  refs/heads/agent/implement-210-… at 11000ba8925521c8fac8efbc3da81d64c4194bb7
  inspect: git show --stat --oneline 11000ba8925521c8fac8efbc3da81d64c4194bb7
  explicit discard: aethyme broker finish cleanup 493 --force
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
aethyme broker finish cleanup --all-cleaned --apply --confirm <plan-sha256>
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
aethyme broker finish cleanup <session-id> --force
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

Two gaps recorded by older issues now have guarded paths:

- The gate-cache gap from #295 is covered: caches under the OS cache
  directory appear in `gc plan` and digest-bound `gc apply`. A running
  gate's cache remains protected unless an operator explicitly asks for the
  active-cache lane.
- The markerless-root gap from #257 is covered by host inventory and
  evidence-based attribution. From the owning repository,
  `aethyme broker gc storage attribute` previews markers it can prove from
  Git registrations; `--apply` writes only those markers and removes
  nothing. Roots whose owner is still uncertain remain visible and report-only.

A reclaimable-bytes figure near zero is still a statement about what the
selected plan can prove and reclaim, not about the disk as a whole.

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
