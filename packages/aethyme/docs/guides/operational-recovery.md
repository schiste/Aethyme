# Installation and integration recovery

Last Updated: 2026-09-18

Recovery is a reviewed operation, not an automatic reset. Installation coherence,
repository configuration, and integration ancestry are separate diagnoses.
Fixing one does not prove the others healthy.

## 1. Establish installation provenance

```sh
command -v aethyme
command -v aethyme-engine-cli
aethyme --version
aethyme-engine-cli --version
aethyme plugin status
```

Compare the full build identities, including `build_commit`, not just `0.x.y`.
Different build dates are normal; different commits are not. PATH may select a
different binary than the one you just installed. Do not treat a missing or
unreadable engine as proof that the pair is aligned.

Use one installation method for both binaries. For Homebrew, update the Aethyme
formula. For an installer-managed release, review `aethyme update plan` and
confirm its exact digest. The release installer activates the pair through one
version link and retains the previous bundle for rollback.

For source development, use the same reviewed, committed source tree for both:

```sh
cargo install --locked --path packages/aethyme/rust/crates/aethyme-cli &&
cargo install --locked --path packages/aethyme/rust/crates/aethyme-engine
aethyme --version
aethyme-engine-cli --version
```

Coordinate this host-wide operation with other agents first. Sequential Cargo
installation is **not atomic**: if the second install fails, stop broker work and
finish repairing the pair. Do not report the first successful install as success.
`broker doctor --fix-version` targets integration; do not use it blindly when
integration is divergent or contains work that should not be deployed.

## 2. Verify configuration without changing it

```sh
aethyme certify --json
aethyme readiness --json
aethyme enhance verify --repo .
aethyme graph status --repo . --json
```

Certification validates review policies using their runtime loaders. Unknown
future settings remain warnings; malformed supported settings are errors.
`enhance verify` checks generated-file presence/substitution, not full operational
readiness. Graph-disabled is legitimate and must not require enrolling a graph
merely to obtain navigation. Bounded source hints are not impact evidence.

For generated deployment drift, inspect `aethyme readiness plan --repo . --diff`
before applying any repair. Keep overrides in their source configuration, never
edit generated AGENTS.md or skills by hand. A repair plan does not authorize
discarding working-tree changes.

## 3. Diagnose integration before moving refs

```sh
aethyme broker status --json
aethyme broker integration status --json
aethyme broker integration reconcile --upstream origin/main --dry-run
```

The dry run classifies each promoted delta. `already_landed` is evidence, not a
request to replay it. `still_pending` means the delta survives and must be
preserved. `genuinely_conflicting` requires content review with the owning session.
Never mark pending work superseded just to clear a blocker.

When automatic evidence is insufficient, follow the dry run's commit-bound
resolution-file instructions. `replaced_by_exact_upstream_sha` requires a full,
reachable upstream SHA and review proving replacement. `preserve_and_replay`
retains the promoted delta; `drop_because_content_empty` is valid only when the
broker proves that condition. Inspect the exact new plan and confirmation digest
after supplying evidence. If either head moves, review a new plan.

Do not reset integration/default branches, merge main reflexively into every
session, edit the broker database, close another agent's session, or delete a
worktree to force progress. A blocked reconciliation is a safe stopping point.
Continue independent implementation in a registered isolated worktree, but report
that submission/publication remains blocked. For ambiguous remote outcomes,
inspect external state and reconcile the recorded operation; never blindly retry.

## 4. Prove recovery

After authorized repair, repeat version, readiness, and integration inspections.
Then run `aethyme broker quick-test --with-gate --json` to prove promotion and
failing-gate rejection in a disposable repository. Preserve the exact revision,
test result, outstanding warnings and next action in the handoff. A successful
smoke does not certify pending integration changes or authorize publication.

No step in this guide authorizes source deletion, force-push, release publication,
or external contact. The operator's task and broker coordination still govern
those actions.
