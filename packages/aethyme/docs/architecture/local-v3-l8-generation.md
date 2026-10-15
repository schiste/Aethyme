# Local v3: accepted source generation preparation

Last Updated: 2026-10-15

> Historical checkpoint: the implementation and pending-work statements below
> describe the original provisional profile. Current bounded E1, FX08, native-gate
> and appointed-controller results are recorded in the
> [Local qualification ledger](local-v3-qualification.md). Each original fixture
> and profile keeps its own evidence and limits.


Issue: #677. Plan: sections 6.6, 10.5 GEN and 15.1 L8; D21/D28.
This document records the independently reviewed implementation contract. The
provisional profile is implemented and qualified against the controlled Local schedules below.

## Authority and source

Preparation consumes the private current acceptance capability from #675. A
serialized acceptance receipt, notification or imported bundle is history.
Preparation pins its controller target, target revision, authority epoch,
qualification, provider representation and exact accepted candidate snapshot.
Execution preparation may transform build inputs; the new agent workspace uses
the accepted candidate itself, never that transformed source.

The generation manager rechecks native provider acceptance and verifier trust
at preparation, checks the exact owner/revision/phase and expiry at the final
commit, and preserves the provider-to-source mapping. Source movement, stop,
producer revocation or authority restore prevents a prepared receipt.

## Durable lifecycle

`GEN_INTENT` records the operation, old session/root reference, explicitly
retained pending work and a unique generation identity before source writes.
Separate generation roots protect source, original pending contributions and
mapping records independently of the active target's lifetime. Existing archive
and GC machinery remain the source store and reclamation authority.

The manager creates a new private absolute directory. It reconstructs raw CAS
bytes, verifies the complete file set and modes, and creates standalone Git
metadata using the existing raw Git plumbing. Its local commit is an explicit
synthetic representation; the recorded provider landing commit remains a
separate field. No hooks, remotes, alternates or checkout filters are copied.
Files and Git metadata are synced before `GEN_PREPARED` is committed.

An interrupted directory is an incomplete alternative. Recovery must verify it
or allocate a new explicit operation; it must never overwrite or silently adopt
changed files. Loading a prepared record rechecks its archived identity and
actual directory. A prepared receipt is an offer, not an executor acknowledgment.

## Old work and adoption

Preparation leaves the original absolute workspace intact. Pending-work
references describe what was retained at observation, not a lock on subsequent
edits. Simultaneous edits, open descriptors, watchers and children remain attached
to the old generation until their executor's explicit protocol accounts for them.

#678 supplies the separate controlled-command handoff and actual generation
acknowledgment. Neither preparation nor an event changes another process's
working directory. Retiring an old workspace requires separate accounting and
cleanup authorization; no shared `current` symlink is switched.

## Qualification

Required schedules cover old-generation edits, provider movement, target and
authority changes during materialization, interrupted writes/commit, GC after
target-root release, reconstruction after original Git objects disappear, and
unexpected files/modes/symlink destinations. Golden identities come from an
independent oracle. Each deliberate guard removal must fail its named test.

The profile `aethyme.generation.raw-standalone-git/provisional-v0` has sixteen
passing generation schedules. These cover complete source and Git metadata
checks, prior dirty generation mapping, symlink ancestors, raw CAS reconstruction
after the original Git repository disappears, current provider/producer/target
checks, retained roots during real GC reclamation, and actual late child writes
and watcher reads attached to the old absolute workspace. Crash injection covers
interruptions before source writes and after synced files; this is an in-process
injection, not process-kill or power-loss evidence. The GC test explicitly removes
target roots to simulate their release; it does not claim the target release API
accepts a verifying target.

The prepared identity vector was produced independently with Ruby and has digest
`sha256:d94a1bdcf2f11b7dbdc79aff155a938c7e1f220389e426c3f4b2591236a754b1`.
Independent adversarial review cleared the production fixes. Nine deliberate
removals each failed their named regression: canonical identity, journal state,
prior-generation support, ancestor checks, complete source, Git metadata,
partial-directory refusal, generation GC roots and the final native guard.
Exact sources were restored; 397 selected tests passed with six intentionally
ignored helpers, along with strict Clippy. Collaboration schema/floor is 14.
No held-out composition cases were rerun.

Prepared records remain historical offers. Live GitHub/Mergify qualification
awaits the designated Playground repository and policy. E1 remains junior-owned;
composition profiles remain provisional until its evidence lands. Native gate
integration waits for #725. Full L8 adoption and Q4 are not claimed here.
