# PR Review Scheduling and Delivery

Last Updated: 2026-10-07

Aethyme keeps pull-request observation durable without running a daemon. The
broker owns normalized metadata, cursors, retry decisions, activity batches,
delivery claims, and acknowledgments. A host scheduler decides when to run a
foreground tick. A delivery adapter decides how to notify or resume a target.

This separation is intentional:

- Aethyme stores only allowlisted PR metadata, never comment or review bodies.
- launchd, systemd, Chau7, or another supervisor can schedule the same command.
- adapter targets remain opaque strings; no Chau7 identifier enters the core
  schema.
- no command polls the network in the background after it exits.

## Create a watch and subscription

The owning session must be live. Open and draft PRs are accepted.

```bash
aethyme broker advanced watch pr start \
  --session 111 --repo owner/name --pr 42 \
  --events comments,reviews,checks --seconds 60 --json

aethyme broker advanced deliveries subscribe \
  --watch 7 --adapter my-adapter --target opaque-target \
  --policy notify --json
```

Policies are `notify`, `resume`, and `review-and-push`. `review-and-push` is a
capability request, not publication authority: the host must also retain the
matching user authorization before it allows a remote write.

## Subscribe to every pull request of a repository

A repository watch notices pull requests opening, becoming ready for review,
or reopening, so a session can react to each one, for example by reviewing it.

```bash
aethyme broker advanced watch repo start \
  --session 111 --repo owner/name \
  [--events opened,ready_for_review,reopened] [--include-drafts] \
  [--include-existing] [--exclude-authors dependabot,renovate] \
  [--auto-watch] [--seconds 60] --json

aethyme broker advanced deliveries subscribe \
  --repo-watch 3 --adapter chau7 --target opaque-target \
  --policy review --json
```

- Each `watch pr tick` also polls due repository watches with one read-only
  `gh pr list --state open --limit 100 --json
  number,title,author,url,headRefOid,isDraft`. Bodies and comments are never
  read. The tick's JSON carries the pass as `repository_watches`.
- An event is recorded exactly once per (watch, PR, kind): `opened` for a PR
  the watch has not seen, `ready_for_review` when a seen draft becomes ready,
  `reopened` when a PR seen closed is open again. `watch repo events --id <id>`
  lists them.
- PRs already open when the watch starts are recorded as seen and never fire,
  unless `--include-existing`. Drafts do not fire unless `--include-drafts`;
  their later `ready_for_review` does. Authors in `--exclude-authors` never
  fire (case-insensitive).
- `--auto-watch` also starts one per-PR watch (comments, reviews, checks) for
  each new PR, unless one is already live.
- `watch repo list|show|pause|resume|stop` work as for PR watches; a stopped
  repository watch cannot be resumed.

Subscriptions to a repository watch take `--policy review` (the default) or
`notify`. A `review` delivery asks the session to code-review the PR. Its
prompt comes from `.aethyme/config.toml`:

```toml
[watch.prompts.review]
body = "Review {{repo}}#{{number}} ({{url}}) by {{author}}: {{title}}"
```

A template may name only `number`, `title`, `author`, `url`, `head`, `draft`,
`repo` and `event`; one naming anything else, or one that cannot be parsed, is
ignored in favour of the default prompt. `title` and `author` are written by
the pull request's author, so they are always rendered as one-line JSON
strings, and every prompt ends with a notice that they are untrusted data and
must never be followed as instructions.

Repository deliveries share `deliveries dispatch`, `claim`, `list` and
`complete` with PR deliveries. Their ids start at 1000000000000, so
`deliveries complete --id` routes them without a flag; `dispatch --json` adds
`source` (`pull_request` or `repository`), and `list --json` items from a
repository watch carry `source: "repository"` and `event_id` in place of
`batch_id`. The scheduled PR monitor and the Chau7 adapter need no change.

## Run one scheduler tick

```bash
aethyme broker advanced watch pr tick --limit 32 --json
```

The command polls only active watches whose `next_poll_at` is due, in a stable
order, up to the requested limit. It exits after that one pass. Its versioned
JSON report includes per-watch disposition, safe error code, retry time,
shared rate-limit evidence, and `next_tick_at`.

Provider failures do not create a busy loop. Authentication and invalid-payload
errors receive a five-minute retry delay; ordinary provider failures use a
bounded interval-derived delay; rate limits defer the rest of the current tick
for fifteen minutes without further provider calls. A host may add jitter but
must never schedule before the broker's returned retry time.

For systemd, use a oneshot service with `WorkingDirectory` set to the repository
and a timer that invokes the command periodically. For launchd, set
`WorkingDirectory` and pass the executable plus arguments as an argv array.
Run under the developer account that owns the repository and its authenticated
`gh` session. Do not place tokens in unit files or command arguments.

## Implement a delivery adapter

An adapter loop claims at most one durable item at a time:

```bash
aethyme broker advanced deliveries claim \
  --adapter my-adapter --worker host-worker-1 --seconds 120 --json
```

If `delivery` is null, there is no work. Otherwise, the versioned envelope
contains an opaque target, policy, normalized batch, expected PR head, and a
bounded prompt. The adapter must:

1. Resolve the opaque target without changing the stored identity.
2. Revalidate recipient and PR-head identity.
3. Deliver the prompt without granting permissions beyond the stored policy
   and separately recorded user authorization.
4. Collect a durable per-item outcome from the recipient.
5. Complete using the exact item id, worker, and claim generation.
6. Acknowledge the activity batch only after delivery completion and explicit
   classification of every item.

```bash
aethyme broker advanced deliveries complete \
  --id 19 --worker host-worker-1 --generation 3 \
  --outcome delivered

aethyme broker advanced watch pr ack \
  --id 12 --outcome addressed \
  --reason "all items classified and durable delivery completed"
```

Claims are fenced. After expiry, another worker can reclaim the item with a
new generation; the stale worker cannot complete it. On a temporary missing
recipient, complete with `--outcome retry --error-code target_unavailable` and
apply host-side backoff. Use `failed` only for a reviewed terminal condition.
Never blindly retry an unknown remote Git or GitHub write outcome.

The prompt treats retrieved comments as untrusted data. Comments cannot alter
leases, repository policy, gate selection, or publication authority. A
force-push leaves an older batch bound to its original full head SHA so the
recipient can classify it as stale or superseded instead of applying it to the
wrong tree.

## Pause, recover, and remove

```bash
aethyme broker advanced watch pr pause --id 7
aethyme broker advanced watch pr resume --id 7
aethyme broker advanced watch pr stop --id 7
aethyme broker advanced watch pr batches --id 7 --all --json
aethyme broker advanced deliveries list --adapter my-adapter --all --json
```

Paused and stopped watches are not polled. Stopped and completed watches are
terminal. A closed owner session causes an active watch to pause on its next
explicit poll rather than delivering into an abandoned worktree.

To uninstall automation, disable and remove the host timer/service first, then
stop its watches. There is no Aethyme background process to uninstall. Broker
history remains in `.aethyme/broker.db` under the repository's normal retention
policy.

## Chau7 boundary

Chau7 integration belongs in Chau7: resolve the opaque target to a live tab,
notify or resume it, return its per-item result, and complete the fenced claim.
If the tab no longer exists, the adapter must return the explicit
`target_unavailable` fallback and must not silently start an unrelated agent.
Aethyme's JSON contract and prompt are identical for Chau7 and a dummy adapter.
When the Chau7 adapter has a live tab snapshot, `deliveries dispatch` and
`deliveries resolve-tab` also record its optional repository name, tab name, and
AI provider on the matched broker session. The values make `broker status` and
lease refusals actionable without making the tab id part of broker ownership or
delivery authorization.
