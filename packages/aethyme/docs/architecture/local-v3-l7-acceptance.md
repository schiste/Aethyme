# Provisional Local source acceptance

Last Updated: 2026-10-11

Issue #675; profile `aethyme.acceptance.exact-source/provisional-v0`.
This draft builds on #745. Composition remains provisional until E1 #650.

## Native boundary

An appointed Local controller pins its exact transport intent, revision, authority
epoch, remote route, base branch, committed configuration, Mergify configuration
when selected, and verifier policy. `appoint` reads the real remote base and
fetches its exact object through the existing broker Git journal. The policy
reader verifies raw Git objects; replacement refs cannot change trusted policy.
Review and publication policy parsing reuse the ordinary broker parsers.

`observe` reads GitHub.com through the native `gh` client with a finite deadline,
finite pages, and a maximum decoded response size. It requires the exact PR,
repository, head and base. Before a PR is merged its `merge_commit_sha` is a
synthetic test merge and is never accepted. After a real merge, the accepted
commit is fetched without tracking-ref or FETCH_HEAD writes. Its complete raw
source snapshot and tree must equal the retained candidate. This supports a
changed commit representation for merge, squash or rebase; source changes
require revalidation. A moved base branch produces `target_drift`.

The Mergify route requires committed Mergify configuration and an explicit
appointed merger login. Mergify and GitHub apply their own policies; Local does
not create a second queue policy, change branch protection, or run merges.
Configured exact-head approvals or check-run review evidence are additionally
checked using the existing review policy. Missing, dismissed, stale, duplicate
or wrong-app evidence cannot grant acceptance.

Generic Git supports only the operator-appointed exact conditional ref route.
A configured mandatory review policy makes that route unsupported. Observing
ref availability on a different route cannot stand in for mandatory review.
This profile retains #674's identical fetch/push endpoint and SHA-1 limits.

## Historical observations and current permission

Canonical bindings and receipts are historical data. `load` and `last_receipt`
verify canonical bytes, subjects and journal indexes; neither creates permission.
An interrupted observation consumes its reserved finite slot. Appointment pins
1–64 observations; no observer retries publication or a merge. Ambiguous writes
stay in the original broker operation journal and must be reconciled there.

`qualify` obtains a new native observation, then checks the private trusted
verifier evidence against the current producer, policy, epoch, complete assertion
inventory, artifact and execution snapshot. Producer and authority locks remain
held through the qualification commit. Expiry is checked again before commit.
The resulting permission is private and cannot be deserialized from a receipt.
The controller's explicit final disposition is integrated with generation handoff
separately. Ordinary ship publication and its review lifecycle remain intact.

Collaboration state schema/floor 13 adds the acceptance journal. Quiesce older
open clients before upgrade; the floor guards fresh opens. Local target roots
retain policy-binding and qualification records; the target's candidate roots
remain the source retention authority. Restore checkpoints include these Local
tables under the existing authority mechanism.

## Qualification limits

This package includes controlled provider traces and a real native generic Git
route in disposable Playground repositories. Those traces qualify state and
mapping handling; they do not qualify live GitHub or Mergify deployment.
Live qualification requires a designated Playground repository, target branch
and existing policy. No Aethyme repository is used as an evaluation target.

The native client/credentials, Git service and same-user controller are trusted.
The provider deadline bounds native API calls, not all local raw object hashing.
The shared process helper collects output before the decoded-size check; this
profile makes no whole-process memory bound claim. Native branch observations
are point-in-time witnesses, not a lock on external branch movement.

## References

- [GitHub pull request REST API](https://docs.github.com/en/rest/pulls/pulls#get-a-pull-request): merged state and landing commit representation.
- [GitHub check-run REST API](https://docs.github.com/en/rest/checks/runs#list-check-runs-for-a-git-reference): exact head and app identity.
- [Git fetch](https://git-scm.com/docs/git-fetch): fetch without changing tracking refs or FETCH_HEAD.
