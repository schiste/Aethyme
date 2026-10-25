# Base-failure incident records

Last Updated: 2026-10-25

The broker library can record a confirmed test or build failure on the exact
fetched default tree and assign one repair owner. This is an advisory registry:
it never makes a failing gate pass, skips execution, grants cache authority,
changes cleanup eligibility or authorizes a process signal.

## Confirmation

`Broker::confirm_base_failure_incident` takes an open owner session, a recorded
failed gate run, 1–64 operator-supplied failing test identifiers, and an optional
positive repair PR number. Identifiers are sorted and deduplicated, limited to
200 bytes each, and cannot contain control characters or surrounding whitespace.
The PR number is a local annotation; no GitHub lookup or ownership proof is
claimed.

The broker requires an explicit `refs/remotes/origin/HEAD` symbolic default,
resolves its fetched commit and tree offline, and loads the gate definition from
that committed tree. It refuses a run for another tree or definition, a cleared
failure, a zero or missing exit code, a non-test/build failure, or a missing or
malformed execution-profile digest. Gates that do not record profile provenance
cannot create qualified incidents through this API. No log parser guesses test
identifiers. A local gate row records execution; it is not a signed attestation.

The observation is only as fresh as the last fetch. Moving a Git ref and writing
SQLite cannot be one atomic transaction: the stored base identifies the snapshot
observed by the command, not a guarantee that the remote remains at that commit.
Custom remote-default discovery and current-branch upstream fallback are not
supported by this first API.

An immediate SQLite transaction checks the live owner and gate row and chooses
the incident identity `(base commit, gate name, definition hash, profile hash)`.
The first confirmation owns it. Duplicate confirmation returns that exact
incident, preserving its owner, test identifiers and PR annotation. A quiet or
closed owner is never silently replaced. Explicit ownership handoff is future
work; a closed session does not erase an unresolved incident.

## Resolution and retention

`Broker::resolve_base_failure_incident` requires a newer recorded passing gate
run on the currently observed fetched default tree, with exit zero and the same
gate name, definition hash and profile. A passing feature candidate or different
profile cannot resolve it. Gate-definition changes require a fresh diagnosis;
this API cannot infer equivalence. Repeating the same completed resolution is
idempotent; a different resolution proof is refused.

The table retains at most 128 open and 128 resolved records. Open capacity
exhaustion refuses insertion; unresolved records are never automatically dropped.
Each resolution and insertion prunes older resolved metadata. A resolution may
return the exact resolved snapshot after that older incident is pruned. Reads
are bounded and do not inspect any session worktree or process. Records copy the
gate/tree/profile identity so normal gate and session history reclamation is
not blocked; retained integer IDs are provenance labels, never process authority.

Migration 56 adds only the advisory table and indexes. It does not raise the
compatibility floor or change old writer behavior. The recorded-object repair
pass adopts or repairs the new table idempotently.

## Delivery and qualification

This change is the registry and broker-library boundary. CLI confirmation,
current-base classification, start/pre-push/gate notices, ownership handoff and
automatic resolution after a proven base run still need integration. Issue #760
remains open. The existing path-overlap warnings and generated-file claims do
not provide those incident notices.

Deterministic tests cover duplicate ownership, unqualified/mismatched proof
refusals, matching resolution and retry, open capacity, and schema repair. Native
execution is deferred to remote CI and the final combined local qualification;
no local Cargo or live base-failure experiment is claimed by source inspection.
