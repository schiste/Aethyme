# Upgrading to Aethyme v0.7.2

Last Updated: 2026-09-02

v0.7.2 adds durable pull-request activity observation and a provider-neutral
delivery outbox. Delivery clients such as Chau7 can notify the exact agent
session without becoming responsible for GitHub polling, deduplication,
authorization, or retry state.

## Compatibility

| Contract | v0.7.2 |
| --- | --- |
| Supported platforms | Apple Silicon macOS, Intel macOS, x86-64 Linux |
| Required executables | `aethyme`, `aethyme-engine-cli` from the same build |
| Minimum Git | 2.38 |
| Engine daemon protocol | 1 |
| Broker storage | reads/migrates schemas 1 through 28; writes schema 28 |
| Repository deployment | schema 1; no mandatory migration from v0.7.1 |
| Release channel | `stable` |

The signed release manifest binds these values to the exact source SHA and to
the sizes and SHA-256 digests of both binaries in every archive.

## What changed

- `broker watch pr` creates and manages durable metadata-only pull-request
  watches for comments, reviews, and checks.
- Repeated provider observations are normalized into deterministic activity
  batches. A new watch records existing activity as its baseline.
- `broker deliveries` manages transport-neutral subscriptions, outbox
  inspection, fenced claims, completion, and retry.
- Delivery policies distinguish notification from explicit authorization to
  make minimal fixes and push to the same pull-request branch.
- Generated prompts require repository, PR, head, and session verification;
  they do not contain comment or review bodies and do not authorize merge,
  close, release, force-push, or hook bypass.

## Before upgrading

Finish active sessions when practical and confirm both installed executables
come from the same installation manager. If rollback to v0.7.1 must remain
possible, make a recoverable copy of `.aethyme/broker.db` while no broker
command is running. The first v0.7.2 broker command upgrades it to schema 28,
which v0.7.1 cannot open.

No generated repository file must change merely to install v0.7.2. Inspect any
repository-owned migration separately and locally:

```bash
aethyme upgrade plan --repo . --diff
```

## Install or update

Homebrew updates the paired binaries as one unit:

```bash
brew update
brew upgrade aethyme
```

Installer-managed users should review and confirm the signed manifest plan:

```bash
aethyme update check
aethyme update plan --channel stable
aethyme update execute --confirm <manifest-sha256>
```

## Migrate and verify

The broker database migration is transactional and runs automatically when a
v0.7.2 broker command first opens the repository. Repository deployment stays
at schema 1, so apply no repository write unless an upgrade plan proposes an
exact reviewed diff.

```bash
aethyme --version
aethyme-engine-cli --version
aethyme broker quick-test
aethyme enhance verify --repo .
aethyme upgrade plan --repo . --diff
```

To connect a delivery client, create a PR watch, then subscribe an adapter to
the returned watch ID. Adapter targets are opaque to Aethyme and interpreted
only by that delivery client:

```bash
aethyme broker watch pr start --session <session-id> \
  --repo owner/name --pr <number> --events comments,reviews,checks
aethyme broker deliveries subscribe --watch <watch-id> \
  --adapter <adapter> --target <opaque-target> --policy notify
```

## Rollback

Restore both v0.7.1 binaries together through the original installation
manager and restore the pre-upgrade `.aethyme/broker.db` copy. A v0.7.1 broker
cannot open storage already migrated to schema 28. Never combine a v0.7.1
router with a v0.7.2 engine sibling.

Repository deployment and engine protocol are unchanged, so repository files
need no rollback unless a separate digest-confirmed repository migration was
explicitly applied.

## Known issues

- Windows and Linux ARM archives are not published.
- Update checks and PR polling are explicit; Aethyme performs no background
  network request.
- Homebrew installations must be upgraded through Homebrew.
- Broker-storage migrations are forward-only; downgrade requires the
  pre-upgrade database copy described above.
- Delivery is at-least-once. Adapters should include the delivery ID in their
  user-visible message and rely on claim fencing for safe retry handling.
