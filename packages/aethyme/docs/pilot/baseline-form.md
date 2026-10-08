# External pilot baseline form (protocol v1)

Last Updated: 2026-10-08

Complete one copy per participating repository before installing or enrolling
Aethyme. Use a random pilot code that does not reveal a company, team, or
repository. This form records aggregate observations only; do not enter names,
repository URLs, paths, task descriptions, commit ids, source, or secrets.

This is an operational research form, not a legal consent agreement. The pilot
owner must obtain any consent and privacy review required by the participant's
organization before collecting or sharing answers.

## Consent choices

Explain the study and its three-week schedule before collecting the form.
Participation, sharing reviewed aggregate results, sharing redacted interview
notes, and publishing a quotation are separate choices. No answer is required
for a choice the participant declines.

- Pilot code: ____________________
- I agree to take part in the one-week baseline and two-week follow-up: yes / no
- I agree to share reviewed aggregate numeric results: yes / no
- I agree to share redacted interview answers: yes / no
- I agree to publish this exact quotation, if one is proposed separately: yes / no
- Aggregate retention period and deletion date disclosed by pilot owner: ____________________
- Withdrawal contact supplied by pilot owner: ____________________
- Consent date (local record only): ____________________

Do not put participant names or contact details in the copy used for
aggregation. A participant may withdraw at any time; remove their shared
artifacts on request and exclude their observations from later reports.

## Repository profile

- Pilot code: ____________________
- Protocol version: `1`
- Aethyme version planned for follow-up: ____________________
- Broker policy / configured-gate version or digest: ____________________
- Promotion mode (`auto`, `verify-only`, or `manual`): ____________________
- OS family and architecture (for example, macOS arm64): ____________________
- Developer count (2–10): ______
- Typical concurrent agents or clones (at least 2): ______
- Existing workflow category (for example, direct branches + CI): ____________________
- CI cost / integration-risk category and how it was assessed: ____________________
- Baseline window (one week): __________ to __________
- Follow-up window (two weeks): __________ to __________

Do not write the repository, organization, agent, or participant names here.
Store the mapping from pilot code to repository separately, access-restricted,
and delete it when the owner no longer needs it to administer withdrawal.

## Baseline observations

Record the same definitions and measurement method that will be used in the
follow-up. Include failures, abandoned tasks, and missing values in the
denominators. Use estimates only when marked as estimates; do not reconstruct
missing timings from task text or source history.

| Measure | Baseline | Aethyme follow-up | Denominator / method / missing-data reason |
| --- | --- | --- | --- |
| Time to first session (minutes) | Existing multi-agent workflow | First broker session | |
| Time from work/session start to first accepted result (minutes) | From task start to accepted integration | From session start to first accepted `submit`; note promotion mode | |
| Task elapsed minutes to accepted integration | | | |
| Human/operator minutes per sampled task | | | |
| Tasks accepted / rejected / abandoned / incomplete | | | |
| Merge or integration conflicts observed | | | |
| Recovery incidents and unaided recoveries | | | |
| CI duration and infrastructure failures | | | |
| Disk growth and cleanup effort | | | |
| Weekly active repositories / retained use | Existing workflow | Aethyme use and week-two retention | |

For the Aethyme follow-up, record time from install start to first broker
session separately from the baseline's first ordinary multi-agent session.
Record the configured promotion mode with every first-accepted-submit measure;
in `verify-only` mode, a successful submit verifies but does not promote.

Observation notes (workflow-level only; no task descriptions or identifying
details):

____________________________________________________________________________

____________________________________________________________________________

## Unaided concept check

Ask before explaining Aethyme. Capture a short, redacted paraphrase or mark
"not yet clear"; do not coach the participant before recording the answer.

- Why does an agent session use its own worktree?
- What does `submit` verify, and what does promotion do under this repository's mode?
- Why is publication a separate authorized step?
- How would the team find and recover from a refused operation?
- What does the team currently trust or distrust about local-only metrics and coordinated writes?
- What value does the team expect before setup? What would make setup not worthwhile?

## Follow-up pairing

At the end of two weeks, complete the same measure table with the same method.
Record the Aethyme version, policy changes, agent-count changes, upgrades,
resets, unusual task mix, missing windows, and human interventions. Compare
per-repository results first; do not pool raw task rows across teams or claim
causation from this small observational sample.

Attach only the numeric `pilot-delta.json` after reviewing it with the
participant. Use [external-pilots.md](../guides/external-pilots.md) for the
analysis, publication, and stop/go rules.
