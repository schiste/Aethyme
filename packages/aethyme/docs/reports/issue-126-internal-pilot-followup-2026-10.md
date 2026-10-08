# Internal pilot follow-up for issue #126 (2026-10-08)

Status: internal-only acceptance evidence following [PR #639](https://github.com/schiste/Aethyme/pull/639). The Aethyme team is the pilot cohort for now; external recruitment remains deferred. This follow-up does not close issue #126 or claim independent adoption, onboarding success, or product-market fit.

## Scope

This exercise adds two acceptance checks missing from the 2026-10-08 internal dogfooding snapshot: a real broker conflict-recovery path in a disposable local repository, and the paired CLI/engine install, update, rollback, and uninstall lifecycle. No Aethyme source repository data was used as the conflict fixture, and no external repository or user was involved.

## Results

| Check | Observed result | Limit |
| --- | --- | --- |
| Conflicting broker submissions | Two isolated sessions committed incompatible edits to the same README line. Session 1 submitted and was promoted. Session 2 was rejected before gates with the conflict identified. | Disposable local repository; no configured gates, so promotion used the broker's conflict-only path. |
| Conflict recovery | `aethyme broker advanced repair` paused at the expected rebase conflict. The line was reconciled, the rebase continued, and session 2 submitted and was promoted. Both sessions were then finished; their worktrees and branches were removed. | This demonstrates the documented operator recovery path, not unaided recovery by a new team member. |
| Paired installation | Release installer installed CLI and engine version `0.8.24` into an isolated temporary prefix; both executables reported the same version. | Release signature verification could not initialize the user Sigstore trust root in this sandbox. The isolated run used the documented signature-verification opt-out while retaining release-manifest archive hash verification. |
| Update | Update plan selected `execute_installer_update` from `0.8.24` to `0.8.25` and returned a manifest digest confirmation. Execution succeeded, both binaries reported `0.8.25`, and the quick test passed. | Same sandbox signature-verification limitation. |
| Downgrade and rollback | The updater refused a direct downgrade to `0.8.24`. Re-running the pinned `0.8.24` installer restored both binaries and retained `0.8.25` in the managed previous-version record. | Rollback was exercised by reinstalling the pinned prior release, not by injecting a mid-update failure. |
| Uninstall | Removed only the executables, managed state, and PATH links in the isolated temporary prefix; the isolated PATH no longer resolved either Aethyme executable. | Did not remove user repositories or unrelated files. |
| First-run broker smoke | `aethyme broker advanced quick-test --with-gate --json` passed: the successful fixture submit promoted, and the failing-gate fixture was rejected; disposable fixtures were removed. | This smoke is separate from the real conflict-recovery scenario above. |

## Decision and remaining evidence

Continue the internal pilot. These checks cover the documented local conflict-recovery path and the normal paired-binary update/rollback/uninstall lifecycle under the stated signature-verification limitation. They do not establish recovery by an unaided first-time operator, behavior after a failed update midway through replacement, a multi-week internal outcome, or independent external adoption. External recruiting remains out of scope unless the maintainer reopens it.
