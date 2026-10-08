# Internal dogfooding snapshot (2026-10-08)

Status: internal-only observational snapshot. The maintainer has deferred
external recruitment; the Aethyme team is the pilot for now. This report does
not claim independent adoption, general onboarding success, or product-market
fit, and it does not complete the original external-validation study in issue
#126.

## Scope and method

This snapshot covers the Aethyme repository's own broker use. Both installed
binaries reported version `0.8.25`. The repository uses `verify-only`
promotion, so a successful `submit` verifies work but does not promote it.

The operational figures below come from `aethyme broker advanced insights
--json` on 2026-10-08, using its 30-day window. Only aggregate values are
recorded here; session rows, task text, paths, and broker databases remain
local. The first-run smoke used `aethyme broker advanced quick-test
--with-gate --json`: its disposable repository completed a successful submit,
rejected a second submit after a fixture gate failed, and was removed. That
smoke uses a disposable fixture and does not exercise real multi-agent conflict
recovery or this repository's `verify-only` policy.

## Observations

| Measure | Internal observation | Limit |
| --- | --- | --- |
| Session funnel | 619 registered, 193 submitted, 165 verified, 72 landed | Stage counts are not a conversion or productivity claim. |
| Registration to first submit | Median 20m 34s; p95 3h 14m | Wall-clock time includes waits; it is not setup time or operator effort. |
| Configured gates | 811 runs; 627 passed (77.3%); 31 cached | The pass rate combines code/test failures and host failures. |
| Cache estimate | About 3h 10m reported saved | This is an estimate, not measured net time saved. |
| Coordination | 11 conflicts caught before gates; 21 out-of-lease-write signals | A caught conflict is not proof of a prevented incident; the write signals need classification before drawing a safety conclusion. |
| Host-class gate failures | 43 resource-contention, 23 timeout, 7 environment failures | These failures materially affect the raw pass rate. |
| Activity-time coverage | 2 of 619 sessions have recorded active time | The remaining 617 predate activity tracking; missing time is unknown, not zero. |
| Pull-request timing | 117 pull requests have an open-time observation; no merge-time observations | Time to merge and retention cannot be reported from this snapshot. |

The disposable smoke demonstrates the basic first-run path and failing-gate
refusal. It does not establish that a maintainer can recover unaided from a
real conflict, installer update/rollback failure, or uninstall. The repository
has no pre-Aethyme baseline or structured internal interview for this window.

The [internal pilot follow-up](issue-126-internal-pilot-followup-2026-10.md)
exercises the documented conflict-recovery path and the paired install,
update, rollback and uninstall lifecycle, within the limits it states.

## Decision

Continue using Aethyme internally as the near-term pilot. Treat the observed
gate contention, timeouts, missing activity-time coverage, and out-of-lease
signals as follow-up evidence; do not count cache estimates or overlap warnings
as proven productivity or avoided incidents. External recruiting is out of
scope for now. Reopen it only if the maintainer later wants independent
adoption evidence; no external claim should be made from this internal sample.
