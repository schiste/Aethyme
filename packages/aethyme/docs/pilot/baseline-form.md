# Internal dogfooding baseline form (protocol v2)

Last Updated: 2026-10-08

Complete one copy for each internal observation window. Record aggregate
workflow details only. Do not enter names, repository URLs, paths, task
descriptions, commit ids, source, or secrets.

## Observation profile

- Internal observation code: ____________________
- Protocol version: `2`
- Aethyme version: ____________________
- Broker policy / configured-gate version or digest: ____________________
- Promotion mode (`auto`, `verify-only`, or `manual`): ____________________
- OS family and architecture: ____________________
- Internal team size: ______
- Typical concurrent agents or clones: ______
- Existing workflow category: ____________________
- CI cost / integration-risk category: ____________________
- Baseline window: __________ to __________
- Aethyme observation window: __________ to __________
- Missing or retrospective baseline data: ____________________

Do not write repository, organization, agent, or team-member names here. Store
any local mapping separately and access-restrict it.

## Workflow observations

Use the same definitions and measurement method in both windows. Include
failures, abandoned work, and missing values in the denominators. Mark estimates
as estimates.

| Measure | Baseline | Aethyme observation | Denominator / method / missing-data reason |
| --- | --- | --- | --- |
| Time to first multi-agent session (minutes) | | | |
| Time from work/session start to first accepted result (minutes) | | | |
| Task elapsed minutes to accepted integration | | | |
| Human/operator minutes per sampled task | | | |
| Tasks accepted / rejected / abandoned / incomplete | | | |
| Merge or integration conflicts observed | | | |
| Recovery incidents and outcomes | | | |
| CI duration and infrastructure failures | | | |
| Disk growth and cleanup effort | | | |
| Active repositories / continued use | | | |

For Aethyme, record setup-to-first-session separately from first-session-to-
first-accepted-submit. Record the configured promotion mode with every submit
measure; in `verify-only` mode, a successful submit verifies but does not
promote.

Workflow notes (no task descriptions or identifying details):

____________________________________________________________________________

____________________________________________________________________________

## Unaided concept check

Ask internal team members to explain these before coaching. Record a short,
redacted paraphrase or mark "not yet clear":

- Why does an agent session use its own worktree?
- What does `submit` verify, and what does promotion do under this repository's mode?
- Why is publication a separate authorized step?
- How does the team find and recover from a refused operation?
- What does the team trust or distrust about local-only metrics and coordinated writes?
- What value is clear before setup, and what remains confusing?

## Follow-up

At the end of the observation window, complete the same measures with the same
method. Record version or policy changes, agent-count changes, upgrades,
resets, unusual task mix, missing windows, and human interventions. Compare
like-for-like internal workflows and do not claim causation from an
observational sample.

See [internal-dogfooding.md](../guides/internal-dogfooding.md) for analysis and
reporting limits.
