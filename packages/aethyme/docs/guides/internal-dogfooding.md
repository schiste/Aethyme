# Internal dogfooding (protocol version 2)

Last Updated: 2026-10-08

Status: The Aethyme team is the pilot cohort. External recruitment is out of
scope and is not required for this work. Findings describe internal use only;
they do not establish independent adoption or product-market fit.

Use the [baseline form](../pilot/baseline-form.md) for a new observation
window, the [install and recovery guide](../pilot/install.md) for disposable
setups, and the [internal pilot kit](../pilot/README.md) for the team workflow.

## Scope and data handling

Use Aethyme's normal project work for operational observations. Use disposable
Playground repositories for conflict, gate-failure, update, rollback, and
uninstall exercises. Do not use Aethyme itself as an evaluation fixture.

The product collects no background telemetry. Metrics export is local and
allowlisted. Review summaries with the team before including them in an internal
report. Keep source, diffs, task text, raw broker databases, usernames, paths,
branch names, and private repository identity out of reports.

## Observation windows

Record a baseline before starting a new comparison window when possible. If
historical baseline data is unavailable, say so; do not reconstruct it from
memory or present an observational comparison as causal evidence. Record the
Aethyme version, policy version, observation dates, missing data, and any
changes in hardware, agent count, dependencies, or CI policy.

For sampled work, record locally:

| Field | Definition |
|---|---|
| Task elapsed minutes | Start of work to accepted, validated integration, including waits |
| Operator minutes | Human coordination and recovery effort, separately from elapsed time |
| Result | Accepted, rejected, abandoned, or incomplete; retain failures in the denominator |
| Conflict and recovery | Incident count, whether caught before integration, and recovery outcome |
| Validation | CI/gate elapsed time, infrastructure failures, and failures missed before integration |
| Disk | Before/after allocated bytes and cleanup effort using the same method |
| Friction | Refused commands, onboarding confusion, and prompt/policy burden |

Report time-to-first-session and time-to-first-accepted-submit separately.
Record the repository's promotion mode: in `verify-only` mode, a successful
submit verifies but does not promote. Gate execution times overlap with task
time; do not add them together. Estimated cache savings are not net savings,
and overlap warnings are not proven prevented incidents.

## Internal workflow

1. Keep ordinary work in isolated broker sessions and record setup, submit,
   gate, recovery, and publication friction.
2. In a disposable Playground repository, exercise a passing submit, a failing
   gate, overlapping edits, and conflict recovery. Record failures and manual
   interventions rather than quietly repairing the test.
3. In a disposable home or checkout, exercise the documented update, rollback,
   and uninstall procedures. Preserve outstanding session work and user
   branches; never use deletion of broker state or worktrees as a shortcut.
4. Ask team members to explain session, lease, integration, submit, and
   publication in their own words before coaching. Record confusion and steps
   that required help.
5. Review aggregate results with the team and file generic product follow-ups
   as separate issues.

If a recovery or deletion outcome is unexplained, stop the exercise and record
it as a failure, not as onboarding friction.

## Metrics export

Use the [local metrics export](../pilot/metrics-export.md) only when it answers
a specific internal question. The numeric allowlist omits task and repository
identity; raw diagnostic snapshots remain local. Advisory gate suggestions are
not configured gate executions and must not be counted as runs.

## Results template

- Aethyme and policy versions; observation windows:
- Internal repositories and workflows observed:
- Time-to-first-session and time-to-first-accepted-submit:
- Gate cost, infrastructure failures, disk growth, and cleanup effort:
- Conflicts, recovery outcomes, and manual interventions:
- Update, rollback, and uninstall outcomes:
- Missing evidence, limitations, and negative results:
- Product decisions supported by evidence (separate implementation issues):

Do not claim independent adoption or product-market fit from this internal
pilot. External testing requires a separate maintainer decision and scope.
