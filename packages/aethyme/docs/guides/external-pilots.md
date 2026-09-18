# External adopter pilots (protocol version 1)

Last Updated: 2026-09-18

Status: ready-to-run protocol and offline tooling, **not completed pilots**.
Three independent repositories must finish before publishing conclusions.
An operator must recruit consenting participants; agents must not invent users,
send invitations, upload reports, or claim adoption without authorization.

## Cohort and consent

Recruit 3–5 teams of 2–10 developers, each using at least two concurrent coding
agents/clones, on macOS or x86-64 Linux. Include both an expensive-CI repository
and a team unfamiliar with Aethyme. Exclude critical production repositories
until cleanup and installer safety fixes have been reviewed and released.

Assign random local pilot codes unrelated to repository or company names.
Obtain explicit consent separately for participation, reviewed aggregate sharing,
and any published quotation. Participation does not require uploading source,
diffs, broker databases, raw metrics, task text, usernames, paths, or secrets.
The tools below never transmit anything. A participant may withdraw at any time;
delete their shared reports on request and state how long aggregates are retained.

## Install and rollback checklist

Record the exact released Aethyme version, OS family, agent count and gate policy
version locally. Use the same versions/policy throughout each comparison window.
Follow [operational recovery](operational-recovery.md) to verify both binaries.
Use a disposable clone for the first install → enroll → start → submit → finish.
Record elapsed time and every manual intervention; do not quietly rescue a user.
Exercise a failing gate and a real conflict, then recovery. Have one participant
exercise update/rollback using the installer-managed previous bundle.

Before uninstall, finish or preserve outstanding session work, inspect integration
and retain user branches. Never remove all host worktrees or broker state as an
uninstall shortcut. Disable integration hooks using the deployed product's documented
uninstall procedure; uninstall binaries through the same package manager that installed
them. Verify ordinary Git operations and source files remain intact.

## Baseline and follow-up

Use a one-week baseline with the team's existing workflow, then two weeks with
Aethyme on comparable tasks. This is an observational comparison, not a randomized
causal experiment. Record differences in task size, agent count, hardware, dependency
changes and CI policy; do not attribute those differences to Aethyme.

For every sampled task, record locally:

| Field | Definition |
|---|---|
| Task elapsed minutes | Start of work to accepted, validated integration, including waits |
| Operator minutes | Human coordination/recovery effort, separately from elapsed time |
| Result | Accepted, rejected, abandoned, or incomplete; never drop failures |
| Conflict/recovery | Incident count, whether caught before integration, and unaided recovery success |
| Validation | CI/gate elapsed time, infrastructure failures, and failures missed before integration |
| Disk | Before/after allocated bytes and cleanup effort using the same measurement method |
| Friction | Refused commands, onboarding confusion and prompt/policy burden |

Do not export task descriptions. Share aggregated timings and counts only after review.
Report time-to-first-session and time-to-first-accepted-promotion independently.
At two weeks record retained use, active repository count and uninstall success.

## Offline numeric export

From a checkout of this protocol, use `jq` to project metrics to a strict numeric
allowlist. Run the broker command in the participant repository and pass the absolute
path of the reviewed filter; the relative example below assumes it is in that checkout.

```sh
aethyme broker metrics --json | jq -e -f scripts/pilot-report.jq > pilot-baseline.json
# After the observation window:
aethyme broker metrics --json | jq -e -f scripts/pilot-report.jq > pilot-followup.json
jq -e -s -f scripts/pilot-compare.jq pilot-baseline.json pilot-followup.json > pilot-delta.json
```

Check every command's exit status and review the files before sharing. The filter
drops custom gate/command names and arbitrary extra fields. Missing, negative,
non-numeric or unsafe-size counters fail instead of turning into zero. A counter
decrease means pruning/reset or incompatible observation windows: start a new baseline.
Keep raw metrics local; only share the reviewed allowlisted export by explicit consent.

These are cumulative instrument counters, not an end-to-end productivity benchmark.
Command and gate execution times overlap and must **not** be added. Estimated cache
savings are not net savings; overlap warnings are not proven prevented incidents.
Snapshot subtraction cannot detect a counter reset that subsequently surpassed its
old value: record resets/upgrades and invalidate those windows manually.

## Interviews and aggregation

After onboarding ask the participant to explain session, lease, integration and
publication in their own words. Ask what they expected at each refusal, where they
needed help, and which steps they would remove. After two weeks ask whether they
would continue without maintainer support and why.

For each repository publish medians and p90 of task elapsed/operator minutes, counts
and denominators for success/failure/recovery, and gate/disk cost. Keep repository
results separate, then report the median of repository-level changes so one busy
team cannot dominate the result. Do not pool incomparable tasks into a savings claim.
Publish cohort size, protocol/product versions, missing data, excluded windows and
all negative results. Small samples do not establish product-market fit.

Success requires three independent completed journeys, one unaided conflict recovery,
one update/rollback, reviewed privacy-safe data, and a prioritized decision report.
Every participant should get an explicit go/stop decision; inability to recover or
unexplained deletion is a stop condition, not an onboarding inconvenience.

## Results template

- Protocol/product versions and observation windows:
- Repositories recruited / completed / withdrawn, with reasons:
- Baseline versus follow-up timings and denominators:
- Gate cost, infrastructure failures, disk growth and cleanup effort:
- Recovery, rollback and uninstall outcomes:
- Interventions needed and retained use after two weeks:
- Missing evidence, limitations and negative results:
- Product decisions supported by evidence (separate implementation issues):

Leave this template unfilled until actual participants supply reviewed observations.
