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
on = ["base_retargeted", "merge_queue_entered"]
areas = ["database"]

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

Dimension names in `require` are free strings. `code`, `security`,
`performance`, `data`, `conventions` are conventions, not an enum -- a
repository that wants `accessibility` or `i18n` simply names it and routes it.

`on` accepts `pull_request_opened`, `ready_for_review`, `reopened`,
`replacement_commit` (force-push, amend, rebase), `additional_commit`,
`base_retargeted`, `review_dismissed`, `merge_queue_entered`, `scheduled`, and
`manual`. A force-push that rewrites the same logical change and a commit
stacked on top of reviewed work justify different responses, which is why they
are separate values rather than one "the head moved".

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
```

| Trailer | Effect |
| --- | --- |
| `Area:` | Matched by a rule's `areas`; labelled `aethyme/area:<value>`. |
| `Surface:` | Matched by a rule's `surfaces`; labelled `aethyme/surface:<value>`. |
| `Risk:` | Compared against `min_risk`; labelled `aethyme/risk:<value>`. |
| `Review:` | Asks for a dimension directly, with no rule required. |

Values are comma-separated and case-insensitive. Trailers may appear anywhere
in the body, and are merged across every commit in the change -- the highest
risk wins, and the areas union.

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

Review spend and live Chau7 tabs are the two inputs it cannot get without a
network, and the `assumptions` array says so rather than guessing. What it
shows is the decision for a pull request with nothing spent yet -- which is the
case an operator is reasoning about while writing rules.

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

## See also

- [`pr-review-delivery.md`](pr-review-delivery.md) -- the durable watch and
  delivery lane for reviews that already exist on a pull request. That feature
  observes a pull request and notifies an adapter; this one decides whether a
  review is owed at all.
- [`broker-workflows.md`](broker-workflows.md) -- sessions, leases, gates, and
  the coordinated `gh` lane.
- [`../reference/cli.md`](../reference/cli.md) -- every broker command.
