# Review Routing

Last Updated: 2026-09-11

Which reviews a change needs, who performs them, and what the pull request
says about it. Three independent tables in `.aethyme/config.toml`:

| Table | Question it answers |
| --- | --- |
| `[review.trigger]` | Does this change need a review, and should one be spent now? |
| `[review.routing]` | Who performs it -- a Chau7 agent, a provider bot, or nobody? |
| `[review.projection]` | What does the pull request show about it? |

Each is read independently. Enabling one does not enable the others, and a
repository may sensibly run projection with no routing (record the decision,
let CI carry the check) or routing with no projection (review quietly).

## The default is that nothing happens

Every table is off by default, at three separate levels:

- A repository with no `.aethyme/config.toml` loads the default policy.
- A `.aethyme/config.toml` with no `[review.*]` table loads the default policy.
- A `[review.*]` table without `enabled = true` is the default policy.

With the default policy, `eligible_types` returns nothing, so nothing is
scheduled, nothing is routed, and nothing is projected. No comment appears, no
label is written, no agent starts. A pull request in a repository that has not
opted in is byte-for-byte identical to one in a repository running a broker
without this feature at all.

Confirm it for any repository without changing anything:

```console
$ aethyme broker review plan --base aethyme/integration --pr 42
{
  "assumptions": [
    "no reviews have been spent on this pull request yet",
    "no Chau7 tabs and no reviews are in flight",
    "the pull request carries no labels and no Aethyme comment"
  ],
  "base": "aethyme/integration",
  "change_root": "/path/to/your/worktree",
  "changed_paths": 25,
  "classification": { "areas": [], "requested": [], "risk": null, "surfaces": [] },
  "decisions": [],
  "dispatch": [],
  "eligible": [],
  "head": "004a802b77b79c2d32d063e831374dc2b752fbbd",
  "performed": false,
  "policy_root": "/path/to/the/repository",
  "projection": [],
  "projection_enabled": false,
  "pull_request": 42,
  "routing_enabled": false,
  "trigger_enabled": false
}
```

`performed: false` is not a property of the dry run alone. **Nothing in this
feature performs anything yet.** What ships is the decision plane: pure
functions from a change to a list of actions, and one command that prints
them. The executor that would take a `spawn_chau7_review` action and start an
agent, or hand an `add_labels` action to `aethyme broker gh`, is not wired up.
An operator can therefore write and tune a policy against real changes, read
exactly what it would do, and run the resulting `gh` commands by hand, with no
risk that a half-written rule acts on a live pull request.

## A complete configuration

Append to `.aethyme/config.toml` at the repository root. Every value below is
explicit, including the ones that match the default, so the file reads as a
decision rather than an omission.

```toml
# ---------------------------------------------------------------------------
# Which reviews a change needs.
# ---------------------------------------------------------------------------
[review.trigger]
enabled = true

# A rule fires when EVERY condition it names holds. A rule that names no
# condition matches every change -- which is how you say "always".
[[review.trigger.rule]]
name = "always-code-review"
require = ["code"]

# Paths are repository-relative, as `git diff --name-only` prints them.
# `*` matches one segment, `**` matches any number of segments including zero.
[[review.trigger.rule]]
name = "security-sensitive-paths"
require = ["security"]
paths = [
  "packages/aethyme/rust/crates/aethyme-broker/src/operations.rs",
  "packages/aethyme/rust/crates/aethyme-broker/src/ship.rs",
  ".github/workflows/**",
]

# Conditions may also read what the author declared, and who the author is.
[[review.trigger.rule]]
name = "untrusted-input"
require = ["security"]
from_fork = true

[[review.trigger.rule]]
name = "first-change-from-a-new-contributor"
require = ["code", "conventions"]
first_time_contributor = true

[[review.trigger.rule]]
name = "the-author-said-this-is-risky"
require = ["security", "performance"]
min_risk = "high"

[[review.trigger.rule]]
name = "schema-changes-on-a-retarget"
require = ["data"]
on = ["base_retargeted", "replacement_commit"]
areas = ["database"]

# A model reviewing its own output has correlated blind spots exactly where
# review is supposed to be independent.
[[review.trigger.rule]]
name = "independent-eyes-on-model-authored-work"
require = ["code"]
models = ["claude-opus-5"]

# How often a dimension may be spent. Rules decide what a change deserves;
# this decides what it gets, and is what keeps a rich rule set from becoming a
# quota bonfire on a branch with forty pushes.
[review.trigger.default_schedule]
debounce_seconds = 600       # absorb a burst of pushes
max_per_pull_request = 8     # 0 = no cap
always_on_new_head = false   # re-review on a new head even past the cap

[review.trigger.schedule.security]
debounce_seconds = 0         # never delay a security review
max_per_pull_request = 0     # and never cap one

# ---------------------------------------------------------------------------
# Who performs a review.
# ---------------------------------------------------------------------------
[review.routing]
enabled = true
workspace_root = ".aethyme/reviews"   # relative paths resolve against the repo root

# A dimension with no route of its own gets this one.
[review.routing.default_route]
backend = "record"

[review.routing.route.security]
backend = "chau7"
max_concurrent = 1            # 0 = unbounded
stale_after_minutes = 360     # give up on an unfinished review after 6h; 0 = never
instructions = "Pay attention to the broker's coordinated-write path."

[review.routing.route.code]
backend = "provider_comment"
mention = "codex"             # stored without the `@`

# ---------------------------------------------------------------------------
# What the pull request shows.
# ---------------------------------------------------------------------------
[review.projection]
enabled = true
comment = true                # one comment Aethyme owns and edits in place
label_prefix = "aethyme/"     # the entire safety mechanism for labels
label_areas = true            # aethyme/area:backend
label_surfaces = true         # aethyme/surface:auth
label_risk = true             # aethyme/risk:high
label_reviews = true          # aethyme/review:security, while one is outstanding
reserved = ["skip-review"]    # read, never written or removed
create_missing_labels = true  # `gh pr edit --add-label` fails on an unknown label
```

A shorter starting point, if the full file is more than the repository needs:

```toml
[review.trigger]
enabled = true

[[review.trigger.rule]]
name = "always-code-review"
require = ["code"]

[review.projection]
enabled = true
```

That records a code review on every change and shows it on the pull request,
with no agent started and no bot mentioned.

## What a rule can condition on

| Field | Type | Meaning |
| --- | --- | --- |
| `require` | list of strings | Review dimensions this rule demands. **Required**; a rule with an empty `require` is rejected at load. |
| `name` | string | Appears in the decision's `because`, so an operator can see which rule spent their quota. |
| `on` | list | Lifecycle transitions this rule fires on. Empty means any. |
| `paths` | list of globs | Matching any one path satisfies the condition. |
| `areas` | list | `Area:` values the author declared. |
| `surfaces` | list | `Surface:` values the author declared. |
| `min_risk` | string | Declared `Risk:` at or above this level. |
| `from_fork` | bool | Only when the change comes from a fork. |
| `first_time_contributor` | bool | Only when the author has not landed here before. |
| `authored_by_model` | bool | `true` only when a `Model:` trailer names one; `false` only when none does. |
| `models` | list | Only when the declared `Model:` is one of these, compared case-insensitively. |

Dimension names in `require` are free strings. `code`, `security`,
`performance`, `data`, `conventions` are conventions, not an enum -- a
repository that wants `accessibility` or `i18n` simply names it and routes it.

`on` accepts `pull_request_opened`, `ready_for_review`, `reopened`,
`replacement_commit` (force-push, amend, rebase), `additional_commit`,
`base_retargeted`, `review_dismissed`, `scheduled`, and `manual`. A force-push
that rewrites the same logical change and a commit stacked on top of reviewed
work justify different responses, which is why they are separate values rather
than one "the head moved".

`merge_queue_entered` is **rejected at load**. Nothing a tick can ask the
provider distinguishes it, so a rule waiting for it would never fire -- and
configuration that looks active and is dead is the exact failure this whole
vocabulary exists to avoid. The error names the rule and the trigger:

```
.aethyme/config.toml: review.trigger rule 3 waits for `merge_queue_entered`,
which no tick can report; remove it from `on` or the rule will never fire
```

`min_risk` ranks `none` below `low`, `low` below `high` and `critical`. An
unrecognised value ranks *above* `low`: a typo in a risk trailer escalates
rather than silently downgrading.

## How the coder supplies classification

A commit trailer. It costs the agent writing the commit nothing -- it is
already writing a commit message, and it already knows what it just changed --
and it needs no extra turn, no API call, and no schema the author has to look
up.

```
feat(broker): coordinate the write path

Problem: ...
Decision: ...

Area: backend
Surface: auth
Risk: high
Review: security
Model: claude-opus-5
```

| Trailer | Effect |
| --- | --- |
| `Area:` | Matched by a rule's `areas`; labelled `aethyme/area:<value>`. |
| `Surface:` | Matched by a rule's `surfaces`; labelled `aethyme/surface:<value>`. |
| `Risk:` | Compared against `min_risk`; labelled `aethyme/risk:<value>`. |
| `Review:` | Asks for a dimension directly, with no rule required. |
| `Model:` | Matched by a rule's `authored_by_model` and `models`. Not labelled: who wrote a change is the author's to disclose, not Aethyme's to publish. |

Values are comma-separated and case-insensitive. Trailers may appear anywhere
in the body, and are merged across every commit in the change -- the highest
risk wins, the areas union, and the first `Model:` declared wins, because a
later commit that names a different model must not be able to relabel work
somebody already declared.

`Model:` is a **declaration, not a detection**. `Co-Authored-By` is the
lookalike and is deliberately not read: it is written by convention, carries a
display name rather than a stable identifier, and appears on commits a model
only helped with. A rule that refuses to let a model review its own output must
not fire on a guess, so this is populated only when an author says so outright
-- and, like every other trailer, it can only ever add a review.

**A declaration can add a review; it can never remove one.** Eligibility is the
union of what the author asked for and what the rules require, never the
intersection. The classification is unverified -- it is one line of text an
author typed -- so treating it as a floor means a mistaken or gamed trailer
costs one unnecessary review and nothing worse. A change to a guarded path gets
its security review whatever the trailer says.

To get agents in a repository to write these trailers, add the instruction to
`.aethyme/overrides/agents.json` and rerun `aethyme enhance deploy --repo
"$PWD"`. Trailers are not part of the default generated agent instructions,
because a repository that has not enabled `[review.trigger]` would be asking
its agents for metadata nothing reads.

## Scheduling, which is a separate question

Eligibility is a pure function of the change. Scheduling is a function of
history, and fusing the two is most of why always-on review over-fires: a
predicate answers *whether* a change deserves a review, but what burns a quota
is *how often*.

`schedule` is consulted per dimension, with `default_schedule` as the fallback:

- `debounce_seconds` -- the minimum gap between two reviews of one dimension on
  one pull request. Inside the window the decision is `defer`, which the next
  tick reconsiders, not `skip`, which is settled.
- `max_per_pull_request` -- the cap that matters on a long-lived branch.
  Without it a pull request with forty pushes spends forty security reviews and
  starves every other pull request in the repository. `0` means no cap.
- `always_on_new_head` -- re-review when the head moves even past the cap. Off
  by default, because the cap exists precisely to survive an active branch.

A dimension that already ran against the current head is skipped outright,
before either of these is consulted.

## Backends

| `backend` | What it does | Needs |
| --- | --- | --- |
| `chau7` | Spawns a terminal agent in its own workspace under `workspace_root`, with a generated prompt naming the pull request, the head, and the dimension. | -- |
| `provider_comment` | Mentions a review bot on the pull request and lets it answer there. | `mention` |
| `record` | Records the request and performs nothing. CI still runs. | -- |

`record` is the default route, because it is the only backend that cannot
surprise an operator with a process or a comment.

`max_concurrent` is per dimension, repository-wide. Separate budgets are what
keep one backlogged dimension from silencing the others: a security review
queue at its limit does not stop a code review being requested.

`stale_after_minutes` is what keeps that budget from being spent permanently.
A slot is released by whoever reports the outcome, and nothing guarantees
anyone does -- a Chau7 tab gets closed, an adapter crashes, a review bot is
uninstalled mid-review. Each tick of `review run` first gives up on any review
of that dimension whose row has not been touched in the window, marks it
`abandoned`, and asks again. It is measured from the last update rather than
from the request, so a reviewer that reported `running` an hour ago is left
alone. `0` disables it, for an operator who would rather wedge than re-ask.

The default is six hours: long enough that a slow review is not interrupted,
short enough that a dead one is not waited on for a working day. Expiries show
up under `expired` in the `review run` report, and one there every tick means
something starts reviews and never reports back -- worth more attention than
the retry it causes.

A Chau7 review whose workspace already has a live tab is deferred rather than
started again. The check is on the directory, not the branch -- a tab that has
wandered onto another branch is still occupying that workspace, which is the
reason not to spawn a second one.

## What the pull request shows

**One comment, edited in place.** Aethyme finds its own comment by an HTML
marker (`<!-- aethyme:review -->`) and edits that one, rather than appending.
The body is a pure function of the record, so re-rendering after a crash is
byte-identical and costs at most a no-op edit.

```markdown
<!-- aethyme:review -->
### Aethyme review

- ○ **code** — requested
- ○ **security** — requested

**Declared by the author**
- Area: backend
- Surface: auth
- Risk: high

<sub>For commit `9f3c1a2b...`. Maintained by Aethyme.</sub>
```

**Labels, namespaced.** Every label Aethyme writes starts with `label_prefix`.
A label outside the prefix is somebody else's and is never touched, which is
what stops the broker getting into a label-flap with CI or with a human over
the same name.

`reserved` names suffixes *inside* the prefix that Aethyme reads and never
writes or removes. This is the distinction between a recomputable label and a
decided one: `aethyme/area:backend` can be deleted and rederived from a trailer
still in the commit, but a human's `aethyme/skip-review` has no source to
rederive from -- once written, it *is* the source. Without `reserved`,
reconciliation would see an unrecognised label under its own prefix and
helpfully delete the only record of that judgement.

## Trying a policy before switching it on

`aethyme broker review plan` reads git and `.aethyme/config.toml`, and prints
what the repository would do. It writes nothing, needs no session, and does not
consult the provider.

```bash
aethyme broker review plan --base aethyme/integration --pr 42
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `--base` | `aethyme/integration` | What to diff against. |
| `--pr` | `0` | Pull request number to plan for. |

Policy comes from the repository's main checkout; the change comes from
whatever worktree you are standing in. Both appear in the report as
`policy_root` and `change_root`, so a surprising verdict can be traced to the
tree it was computed from.

Review spend, live Chau7 tabs, and everything only the provider knows -- the
lifecycle transition, whether the change is from a fork, whether the author is
new -- are the inputs it cannot get without a network, and the `assumptions`
array says so rather than guessing. It plans as if the change were a newly
opened pull request by a known contributor, not from a fork, which is the case
an operator is reasoning about while writing rules. `review run` replaces every
one of those assumptions with a reading.

With the configuration above, against a change touching `operations.rs` and
declaring `Area: backend` / `Surface: auth` / `Risk: high`:

```json
{
  "eligible": [
    { "review_type": "code", "because": ["always-code-review"] },
    { "review_type": "security", "because": ["security-sensitive-paths"] }
  ],
  "decisions": [
    { "action": "request", "review_type": "code", "because": ["always-code-review"] },
    { "action": "request", "review_type": "security", "because": ["security-sensitive-paths"] }
  ],
  "dispatch": [
    {
      "action": "mention_on_pull_request",
      "review_type": "code",
      "pull_request": 42,
      "body": "@codex please review this pull request for **code**. ..."
    },
    {
      "action": "spawn_chau7_review",
      "review_type": "security",
      "pull_request": 42,
      "workspace": "/repo/.aethyme/reviews/pr-42/security",
      "prompt": "Review pull request #42 (head `9f3c1a2b...`) for **security**. ..."
    }
  ],
  "projection": [
    { "action": "create_comment", "body": "<!-- aethyme:review -->\n### Aethyme review\n..." },
    { "action": "create_label", "name": "aethyme/area:backend", "color": "0e8a16", "description": "Declared area: backend" },
    { "action": "create_label", "name": "aethyme/review:code", "color": "5319e7", "description": "Review outstanding: code" },
    { "action": "create_label", "name": "aethyme/review:security", "color": "5319e7", "description": "Review outstanding: security" },
    { "action": "create_label", "name": "aethyme/risk:high", "color": "b60205", "description": "Declared risk: high" },
    { "action": "create_label", "name": "aethyme/surface:auth", "color": "1d76db", "description": "Declared surface: auth" },
    { "action": "add_labels", "names": ["aethyme/area:backend", "aethyme/review:code", "aethyme/review:security", "aethyme/risk:high", "aethyme/surface:auth"] }
  ]
}
```

Every `dispatch` and `projection` entry is a value, not an effect. The broker
decides; a separate adapter performs the transport. `add_labels` carries every
label in one entry so six labels become one `gh pr edit`, rather than six
queued writes behind the same repository lock.

## Performing it

`aethyme broker review run` is `review plan` with the assumptions replaced by
facts, followed by the effects.

```bash
aethyme broker review run --session 408 --repo owner/name --pr 42 \
    --tabs-file /tmp/tabs.json
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `--session` | required unless `--dry-run` | Whose coordinated writes these are. |
| `--repo` | required | `owner/name`, for the ledger and the GitHub writes. |
| `--pr` | required | The pull request to act on. |
| `--base` | `aethyme/integration` | What to diff against. |
| `--tabs-file` | none | A Chau7 `tab_list` snapshot, as JSON. |
| `--from-provider` | off | Read the changed paths and commit trailers from `gh` instead of the working tree, so the command works from anywhere. |
| `--dry-run` | off | Plan against real facts and stop. Needs no session and takes no write lock. |

Each of `review plan`'s three assumptions becomes a reading:

| Assumption | Replaced by |
| --- | --- |
| nothing spent on this pull request | the `review_requests` ledger |
| no reviews in flight | the same ledger, plus the tab snapshot |
| no labels and no Aethyme comment | read-only `gh pr view --json labels,comments` |

An absent `--tabs-file` means "no tabs", which is the safe reading rather than
an error: routing then defers every Chau7 review instead of spawning into a
workspace it cannot see, and the run still records, mentions bots, and projects.
That is what makes `review run` usable from a scheduler with no Chau7 access at
all.

### Where the trigger and the facts come from

`on`, `from_fork`, `first_time_contributor` and `authored_by_model` are facts
about a pull request, not about a diff, so `review run` reads them from the
provider rather than inventing them. Pass `--from-provider` and it takes the
changed paths and the commit trailers from `gh pr view --json files,commits`
too, which is what makes the command usable from a directory that is not the
pull request's checkout.

| Fact | Source |
| --- | --- |
| `on` | compared against `pull_request_observations`, the row holding what the last tick saw |
| `from_fork` | `gh pr view --json isCrossRepository` |
| `first_time_contributor` | `gh api repos/{owner}/{name}/pulls/{n} --jq .author_association` |
| `authored_by_model` | the `Model:` trailer on the change's commits |

The transition is **derived by comparison**, not reported by a webhook. Each
tick fetches the head, base, draft flag, state, and dismissed-review count, and
names the transition against the row the previous tick left behind:

| Seen | Reported |
| --- | --- |
| nothing -- never observed | `pull_request_opened` |
| was closed, now open | `reopened` |
| was draft, now not | `ready_for_review` |
| more dismissed reviews than before | `review_dismissed` |
| a different base | `base_retargeted` |
| a head that descends from the old one | `additional_commit` |
| a head that does not | `replacement_commit` |
| nothing changed | `scheduled` |

Several of these are true at once on a busy pull request -- leaving draft
usually arrives with commits -- and the order above is the precedence: the first
match is the most informative thing to have happened. `git merge-base
--is-ancestor` is what separates a stacked commit from a rewrite, and an
unknown answer reports `replacement_commit`, which is the direction that
re-reviews rather than assumes reviewed work is still reviewed.

The observation is written **after** the tick has acted, never before. Recording
it first would mean a tick that crashed halfway had already declared the
transition handled, and the next tick would derive `scheduled` from its own
unfinished work -- losing the event instead of retrying it. Retrying is cheap
because the ledger's unique index makes a duplicate request a no-op.

### Running it on a schedule

`review run` acts on one pull request. `review tick` sweeps a repository:

```bash
aethyme broker review tick --session 408 --repo owner/name --limit 20 \
    --tabs-file /tmp/tabs.json
```

It lists open pull requests oldest first, runs `review run --from-provider` on
each, and prints one report for the sweep. Oldest first so a repository with
more open pull requests than `--limit` makes progress on a fixed set instead of
re-routing whatever happens to be newest and never reaching the rest. A pull
request that fails is recorded in the report with its error and skipped: one
unreachable pull request must not decide that none of the others get reviewed.

**This is the whole scheduler.** The broker starts no background poller, here or
anywhere else -- a daemon is a second thing to supervise, it holds the
machine-wide database open, and it fails silently. One bounded foreground pass
is something cron, a CI step, a git hook, or a person can run, and its failure
is visible wherever it was run from.

```cron
*/10 * * * * cd /path/to/repo && aethyme broker review tick --session 408 --repo owner/name
```

### Starting the Chau7 reviews

`chau7_handoff` is the one thing the broker hands off, and
`scripts/adapters/chau7-review-adapter.py` is the shipped consumer:

```bash
packages/aethyme/scripts/adapters/chau7-review-adapter.py \
    --session 408 --repo owner/name --repo-path /path/to/repo
```

It takes a `tab_list` snapshot, runs `review tick` with it, and for each handoff
checks the pull request's head out into the workspace, opens a Chau7 tab there,
runs the reviewing agent with the prompt, and closes the row:

- started: `review state --state running`
- could not start: `review state --state abandoned --note "<why>"`, which is the
  one revivable state, so the next tick asks again
- the adapter itself died in between: the row stays `requested` and the route's
  `stale_after_minutes` reclaims it

Every git write it makes -- fetching the pull ref, adding and removing the
review worktree -- runs through `aethyme broker git`, because a review workspace
is a shared-git mutation like any other. The workspace path *is* the identity of
an in-flight review, which is why a workspace already sitting on the right
commit is reused and one sitting on anything else is replaced: reviewing the
wrong commit is worse than not reviewing.

### Which agent performs it

The adapter's `--agent` is a command prefix, and the prompt is appended to it as
one quoted argument. Anything that reviews when handed a prompt on the command
line works; nothing in the broker or the adapter names a model.

Aethyme reviews its own pull requests with Codex Luna, and
`scripts/adapters/codex-luna-review.sh` is the one file that says so:

```bash
packages/aethyme/scripts/adapters/codex-luna-review.sh --session 416
packages/aethyme/scripts/adapters/codex-luna-review.sh --session 416 -- --dry-run
```

It wraps the adapter with three choices worth naming, because each is a failure
you would otherwise diagnose from an empty tab:

- `--approve-for-me`. Nobody is sitting at the tab. A reviewer blocked on an
  approval prompt holds its concurrency slot until `stale_after_minutes`
  reclaims it, and the symptom is a review that never appears rather than one
  that failed.
- `--sandbox workspace-write` with `network_access`. The reviewer must reach
  `gh` to read the diff and the broker to post the result; its checkout is a
  detached throwaway, so writes there cost nothing, and the rest of the
  filesystem is not part of reviewing a pull request.
- A PATH with the machine's `git` wrappers stripped. The broker resolves an
  honest `git` for itself (#176, #178); a reviewer typing `git diff` by hand has
  no such protection and would review bytes nobody wrote.

### Recorded before performed

Every review is written to the ledger before anyone is asked to do it. The
ordering is deliberate and it is not free: a crash between the two costs a
review that was recorded and never performed. The alternative -- perform first,
record after -- costs two reviewers on one pull request, and a missed review is
recoverable because CI still runs.

The ledger's unique index on `(repository, pr_number, review_type,
head_commit)` is what makes that durable rather than merely intended. A
re-running executor writes the same row, learns it already existed, and does not
spawn a second reviewer. `already_recorded` in the report names every review
that took that path.

### The one thing it hands off

GitHub writes go through `run_coordinated_operation`, which is the same
authorization and locking any `aethyme broker gh` takes. Chau7 spawns do not:
the broker has no Chau7 client and does not grow one. They come back in the
report as `chau7_handoff`, for an adapter with Chau7 access to start:

```json
{
  "performed": true,
  "already_recorded": [],
  "github_operations": [
    { "purpose": "request the code review from the provider bot", "operation_id": 91, "success": true },
    { "purpose": "project the review record onto the pull request", "operation_id": 92, "success": true }
  ],
  "chau7_handoff": [
    {
      "review_type": "security",
      "pull_request": 42,
      "workspace": "/repo/.aethyme/reviews/pr-42/security",
      "prompt": "Review pull request #42 (head `9f3c1a2b...`) for **security**. ..."
    }
  ]
}
```

This is the same seam `broker deliveries dispatch` uses: the broker decides, the
caller performs the transport.

A failing GitHub write stops the tick. The rest of the plan describes a pull
request state that write was supposed to establish, so continuing past it would
publish a comment about labels that are not there.

The review that write was asking for goes back to `abandoned` before the tick
stops, and the next run asks for it again. That one line is load-bearing: the
unique index makes a row permanent for its head, so a row left at `requested`
after a failed `gh` call would be read as spend forever and that dimension would
never be reviewed on that commit. `abandoned` is the only state the router may
ask about again.

### Reading the ledger

The ledger is what answers "why was there no security review on #412", months
after anyone remembers the pull request:

```bash
aethyme broker review ledger --repo owner/repo --pr 412
```

```
#412   security   satisfied  chau7            9f3c1a2b4d5e
#412   docs       recorded   record           9f3c1a2b4d5e
        no backend routes docs
#412   code       abandoned  provider_comment 9f3c1a2b4d5e
        the coordinated GitHub write failed: request the code review from the provider bot
```

It is read-only and needs no session. Drop `--pr` for the whole repository.
Each state means one thing and only one:

| State | Meaning | Asked again? |
| --- | --- | --- |
| `requested` | Recorded, nobody has picked it up | holds a slot |
| `running` | A reviewer is working on it | holds a slot |
| `satisfied` | A verdict landed | no |
| `failed` | Attempted, no verdict -- re-asking without a new head buys nothing | no |
| `recorded` | The policy performs nothing here; the row is the whole answer | no |
| `abandoned` | Nobody was ever asked | **yes** |

### Closing a row

`review run` records and decides; it never learns how a review ended, because
the reviewer is a Chau7 tab or a provider bot rather than the broker. Whoever
performed it closes the row:

```bash
aethyme broker review state --repo owner/repo --pr 412 \
    --type security --state satisfied --note "no findings"
```

This is the other half of the `chau7_handoff` seam, and it is the intended way
a slot is released. `stale_after_minutes` is the backstop for when nothing
reports: it bounds how long a dead reviewer can hold a slot, but it costs a
duplicated review every time it fires, so an adapter that reports back is
strictly better than one that relies on it.

`--head <sha>` targets a superseded commit; the default is the most recent
request for that review type, which is what a reviewer reporting now was asked
to do. Reporting on a review that was never requested is an error rather than a
new row: it means the reporter and the router disagree about what was asked
for, and inventing a row would bury that.

## What this never does

- It never performs a GitHub write directly. Every projection action renders
  arguments for `aethyme broker gh --session <id> --repo <owner/name> --reason
  "<authorization>" -- <gh-args>`, which is where authorization and
  coordination live.
- It never touches a label outside `label_prefix`, or a reserved one inside it.
- It never lets a commit trailer remove a review a rule required.
- It never falls back to a permissive default on a policy it cannot read. A
  `schema_version` newer than the broker understands is an error, because
  silently reviewing nothing is the one failure mode this must not have.
- It never merges, pushes, or edits a branch under review. The generated Chau7
  prompt says so explicitly.
- It never starts a background poller. `review tick` is a bounded foreground
  pass; scheduling it is the operator's choice and the operator's cron.
- It never guesses who wrote a change. `authored_by_model` reads a declared
  `Model:` trailer and nothing else.

## See also

- [`pr-review-delivery.md`](pr-review-delivery.md) -- the durable watch and
  delivery lane for reviews that already exist on a pull request. That feature
  observes a pull request and notifies an adapter; this one decides whether a
  review is owed at all.
- [`broker-workflows.md`](broker-workflows.md) -- sessions, leases, gates, and
  the coordinated `gh` lane.
- [`../reference/cli.md`](../reference/cli.md) -- every broker command.
