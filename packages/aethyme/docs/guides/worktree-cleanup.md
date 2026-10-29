# Closed-session worktree cleanup

Last Updated: 2026-10-29

Removing a session worktree is a reviewed operation, with one deterministic
exception described under *Automatic removal* below. A closed session is not
evidence that its work is safe to discard, and a worktree that looks clean can
still hold the only copy of something. The broker refuses by default and names
what it would need; this runbook is how an operator answers it.

The one rule underneath all of it: **decide by whether the content is
represented somewhere else, never by whether the directory looks idle.** Age,
size, and a clean `git status` are all compatible with unique work.

## Lifecycle and proof model

A session, its worktree, and its branch have separate lifecycles. Closing a
session (`finish close`) ends broker ownership and retains both the checkout and
its branch. `finish` closes and then, for a broker-spawned worktree whose cleanup
is already proven safe, removes the checkout and its local session branch in the
same step, unless `--keep-worktree` is given or the repository's retention policy
sets `auto_cleanup_worktrees_on_finish = false`; an adopted worktree is always
retained. Delivery changes the target refs, while cleanup
proves the session's tracked content against the fetched delivery targets. The
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
recoverability evidence. A checkout the report budget did not reach is listed as
`not_inspected` and counted as holding unique work: an unfinished inspection is
never read as clean.

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
candidate. The repository's integration and default branches are reported with
`ownership: shared`, even when an old session record names them, and are never
removable. Branch deletion still needs independent ownership and delivery
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
   protected standalone ignored file. By default every Git-ignored directory
   is disposable, regardless of name or contents; `.DS_Store` and recognized
   artifact files or links and catalogued broker runtime output are disposable
   too. With `strict = true`, every
   ignored leftover keeps the checkout.
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
auto_remove = true             # the default; false turns checkout removal off
strict = false                 # the default; all Git-ignored directories disposable
ignored_directory_sweep = false # opt in to reclaim ignored folders in retained checkouts
regenerable = [".gradle/**"]    # extra artifact-file/link classification
keep = ["*-investigation"]     # worktrees never removed automatically
```

State-only closed checkouts also respect `[retention]
closed_worktree_grace_hours` in `.aethyme/broker.toml` (360 hours / 15 days by default;
`0` disables the window). Automatic checkout cleanup and reviewed whole-checkout
GC share this check; in-place artifact reclamation has its own retention rules.
Automatic apply reloads retention before removal, so a newly raised grace
period or unreadable policy retains the checkout. Explicit `finish cleanup`
remains available without waiting for the window.

Ignored directories are treated as opaque: cleanup does not recursively inspect
package fixtures, keys, databases, dangling links, or saved session records inside
them. Ignoring a directory therefore opts its contents into disposal. Move
irreplaceable records outside disposable checkouts, or enable `strict = true`.
Standalone ignored files such as a root `.env` remain protected. Removal unlinks
symlinks without deleting their external targets.

`ignored_directory_sweep` controls a separate in-place lane for reclaiming
ignored folders from retained closed checkouts. It defaults to `false`; setting
it to `true` can reclaim opaque ignored folders while preserving the checkout,
tracked work, standalone ignored files, and `keep` pins. This in-place lane also
excludes protected Git administration and broker metadata roots from selection.
Metadata and nested repositories inside another disposable ignored directory
remain disposable contents of that directory. It does not shorten
`closed_worktree_grace_hours`. Enable it only after older Aethyme broker
processes have exited and the repository is on a release with the durable
artifact-use protocol. That protocol is available on Unix; unattended ignored
folder and whole-checkout cleanup remain blocked on unsupported targets. Explicit
normal cleanup keeps its existing platform checks there.

Strict mode also disables automatic artifact sweeps, including already queued
background removals. Remove the leftovers manually, or inspect an explicit
reviewed artifact cleanup or force-discard operation. Changing strict mode after
a checkout was selected invalidates that selection before removal; interrupted
cleanup also rechecks the policy. If Git metadata is already gone, strict mode
retains the orphan residue for manual inspection.

With strict mode off, build caches of inactive sessions are still reclaimed
separately, even when a checkout must be kept. `auto_remove = false` disables
whole-checkout removal, while `strict = true` disables unattended ignored-artifact
removal as well.

## Maintenance during broker open

Opening a broker handle may resume an already-authorized durable GC journal
and run the bounded autonomous sweep before the requested command proceeds.
The sweep may reclaim rebuildable artifacts and remove a closed checkout only
when the configured policy and the existing ownership, liveness, cleanliness,
and delivery proofs pass. It does not create a cleanup plan or adopt an
unmarked root; candidates it cannot prove safe remain retained and are
reported by the GC and broker status views.

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

`unproven_provenance` means the broker could not establish the required
representation proof. The work may have landed under a different SHA, or a
bounded history inspection may have been unavailable. This is not evidence that
the work was lost or that deletion is safe.

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

## Preserve unique work before removing a checkout

For one closed, broker-spawned checkout that ordinary cleanup retains:

```sh
aethyme broker finish cleanup resolve <session-id> --archive --json
```

Review the complete plan, then confirm its digest with the same command plus
`--confirm <sha256>`. Recovery preserves commits absent from the listed delivery
targets, staged and unstaged patches, untracked files, and protected standalone
ignored files and links. Disposable ignored directories follow the configured
cleanup policy. Live checkouts, unsafe paths, incomplete inventories and
unsupported nested repositories remain protected.

Resolve plan schema `3` uses a versioned content digest that binds the session,
checkout path, branch ref and tip, HEAD, patch hashes, complete preserved-file
inventory and ignored-content policy. Delivery targets and target-absent commits
are evidence for building the archive, rather than inputs to that content
confirmation. Historical content matching is not needed for preservation:
`landing.state` is `not_evaluated_for_recovery`. This is neither a positive nor a
negative landing verdict. Clean sessions without a direct ancestry proof are
reported as `unproven_provenance`, including work that might have been delivered
by squash or replay. Clean ancestry-proven sessions need no recovery archive and
the public planner directs them to ordinary cleanup.

Recovery archive schema `4` records that evidence snapshot alongside the content
proof; older archive schemas remain readable and verifiable. Before removal,
the broker checks the current content proof, verifies the original archive,
and proves every omitted Git prerequisite remains reachable from a current
delivery target. An upstream fast-forward can therefore preserve eligibility;
changed content, branch state, lost history or unavailable evidence retains the
checkout and archive.

Archive creation and checkout removal are separate results. Inspect
`worktree_removed` and `cleanup_error` even after a successful command, and keep
the reported archive path. Never count an archived-but-retained checkout as
removed or use `--force` to work around a refusal.

## Compact recovery for an exact checkout batch

Keep recoverable history without retaining a full prepared checkout:

```sh
aethyme broker finish cleanup resolve <id-1> <id-2> <id-3> --batch --archive --json
```

Review the complete exact-target plan, then run its `apply_command`. A batch
accepts 1–4096 unique positive session IDs; `--batch` keeps the same command
available when only one target remains. It also permits a delivered closed
checkout to be archived inside the automatic full-checkout grace window.
For a single compact recovery with an item confirmation, use `--retire`.
This is an explicit operator decision; planning alone removes nothing.

The batch confirmation binds the repository, exact IDs, checkout origins and
paths, content confirmations, and adopted-checkout opt-in. Before the first
archive is created, the broker rebuilds every target confirmation. Changed
content or a newly eligible previously blocked item invalidates the batch.
Each target is checked again through ordinary verified recovery before removal.
The byte estimate counts patches and preserved files; commit bundle size is
additional and remains bounded by ordinary archive admission.

Use `--allow-adopted` only when you own the named adopted checkouts and intend
to retire them. Their branches remain. This does not synthesize original
broker ownership or turn an unrecorded directory into a broker-owned checkout.
Unrecorded paths need a separate explicit registration/ownership decision.
Live, unsafe and uninspectable targets remain protected in either mode.

Independent targets continue when another target is protected or fails. JSON
reports each archive/removal outcome and `remaining_session_ids`; any retained
target makes the batch exit nonzero. Replan only those remaining IDs and review
a new confirmation. Never attach an old digest to a reduced or expanded list.
Archives remain subject to the recovery retention policy; do not manually
remove them as regenerable cache or claim an archive-only result as a removed
checkout. The default archive window is 30 days, exceeding the default 15-day
closed-checkout grace.

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

`aethyme broker gc plan` uses a cooperative shared 10-second inspection budget.
An in-flight database query or filesystem call can finish after that deadline.
A plan that reaches the budget, entry cap, or an unavailable inventory reports the
deferred checks and has no apply digest. Use `aethyme broker gc plan
--full-scan` when the inventory is incomplete or needs more time. It removes
the default deadline and artifact-discovery entry cap; discovery still stops
at six directory levels. Then use the emitted
`aethyme broker gc apply` command with the same `--full-scan` option. This
path can take much longer because it measures candidate worktrees and caches
and verifies recovery archives. In particular, the default plan defers the entire recovery
archive inventory whenever its archive directory is nonempty; it does not
hash archive bundles during the bounded scan. If you only need regenerable
build output, `aethyme broker gc reclaim plan` provides the targeted review.

An apply with an existing journal resumes that authorized journal directly;
the planning flags only control the fresh-plan check when no journal exists.

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
