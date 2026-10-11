# Local v3 L7 exact Git source transport (provisional, #674)

Last Updated: 2026-10-11

Profile: `aethyme.source-transport.git/provisional-v0`. Existing broker coordinated
Git operations own command liveness, authorization, hooks, locks and outcomes.
The retained project's state.db owns exact transport intents and canonical past
receipts. There is no second process registry or provider acceptance authority.

## Contract

A native operator constructs `GitCarrier` for one live session, configured remote
and full branch ref, with a finite timeout and explicit authorization reason.
Conditional ref writes require the operator's existing destructive authorization;
foreign live branches remain guarded. This concrete profile requires SHA-1 Git
objects and identical fetch/push endpoints. Remote drift, a foreign broker or
session, stopped targets and stale authority epochs refuse dispatch.

`prepare` binds the canonical intent to project, issuer, authority epoch, active
revision, composed wave, SHA-256 source snapshot, Git commit/tree, native repository
binding, remote identity, ref and expected old object (or absence). Git source and
retained bytes are checked against raw objects, ignoring replacement refs and caller object overrides. Source roots land before the intent is returned.
Missing Git history is reported rather than synthesized.

`publish` marks the intent dispatched before invoking the broker. Each intent
permits one dispatch, including after a crash between the two databases. Another
prepared intent on an uncertain route cannot dispatch before inspection. Local
control mutations wait for the bounded native operation. The returned dispatch
outcome comes from the native journal and grants no acceptance.

`inspect` queries the configured fetch remote directly after establishing identical fetch/push endpoints. This avoids rewriting an already expanded URL a second time and writes no tracking refs. A successful
omitted ref means absent; query errors mean unavailable. Objects on another ref do
not count. A matching advertised ref is availability at observation time, separate
from its historical write outcome. An exact-object fetch writes neither FETCH_HEAD
nor tracking refs, then checks canonical retained source. The receipt states
whether content was checked and names exact native read/write operations and
outcomes. A failed or blocked fetch preserves the ref observation without claiming
checked content. This does not establish remote retention or history durability.

Absent or changed refs cannot prove an earlier write never ran. Transport never
repeats an intent and never reconciles broker ambiguity on the operator's behalf.
Existing explicit reconciliation remains required for unresolved broker operations
before another authorized write. Historical receipts cannot authorize acceptance.
Configured source acceptance belongs to #675. Transport changes neither target
acceptance nor behavioral verification state.

Carrier timeouts bound provider operations and remote identity/ref queries.
Local archive hashing uses the existing raw source reader and is outside that
timeout; this profile does not claim a deadline for the entire API call.

## Storage and limits

State schema 12 adds transport intents and receipt history, floor 12. Earlier
binaries cannot fence unresolved publication against retention release. Quiesce
older processes before migration. Unresolved dispatched revisions cannot release
source roots. The external authority checkpoint includes these Local records.
Import cannot reconstruct current native carrier or issuer authority implicitly.

This is a library boundary. CLI/controller integration is assigned to later Local
packages; typed gate wiring waits for #725. Composer source remains provisional
pending E1. Bare Playground Git tests do not qualify external GitHub/Mergify
acceptance or provider paths. No Artifacts/Workers implementation is included.

## Verification

Sixteen disposable Playground tests cover native conditional publication and
exact-object reads, wrong refs/objects, caller response loss, stopped/old-epoch
fencing, historical record integrity, unresolved retention, native operation
identity, replacement refs, chained URL rewrites and query deadlines/failures.
Independent review found the replacement-ref and URL-rewrite bugs; both are
fixed and re-review cleared the implementation. Fourteen deliberate guard
removals each failed the intended test; source was restored exactly afterwards.
Selected regression checks are in progress.

The exact-object read options follow [Git's fetch documentation](https://git-scm.com/docs/git-fetch):
empty `--refmap` suppresses configured tracking mappings, while
`--no-write-fetch-head` prevents FETCH_HEAD writes. No remote durability or
provider review claim follows from these transport options.
