# PR Review Scheduling and Delivery

Last Updated: 2026-09-03

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
aethyme broker watch pr start \
  --session 111 --repo owner/name --pr 42 \
  --events comments,reviews,checks --seconds 60 --json

aethyme broker deliveries subscribe \
  --watch 7 --adapter my-adapter --target opaque-target \
  --policy notify --json
```

Policies are `notify`, `resume`, and `review-and-push`. `review-and-push` is a
capability request, not publication authority: the host must also retain the
matching user authorization before it allows a remote write.

## Run one scheduler tick

```bash
aethyme broker watch pr tick --limit 32 --json
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
aethyme broker deliveries claim \
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
aethyme broker deliveries complete \
  --id 19 --worker host-worker-1 --generation 3 \
  --outcome delivered

aethyme broker watch pr ack \
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
aethyme broker watch pr pause --id 7
aethyme broker watch pr resume --id 7
aethyme broker watch pr stop --id 7
aethyme broker watch pr batches --id 7 --all --json
aethyme broker deliveries list --adapter my-adapter --all --json
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
