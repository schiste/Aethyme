# Aethyme internal dogfooding kit

Last Updated: 2026-10-08

This kit supports Aethyme maintainers and internal contributors using the
broker in project-owned repositories. The Aethyme team is the pilot cohort;
external recruitment is out of scope and is not an acceptance requirement.
See the [internal dogfooding protocol](../guides/internal-dogfooding.md).

## Team workflow

1. Record a baseline for a new observation window with
   [baseline-form.md](baseline-form.md). Mark historical or missing data
   plainly; do not reconstruct it from memory.
2. Record the exact Aethyme and policy versions using the
   [install and recovery guide](install.md).
3. Route ordinary internal work through broker sessions and record elapsed
   time, operator effort, gates, refusals, and recovery outcomes.
4. Use disposable Playground repositories for conflict, gate-failure,
   update/rollback, and uninstall exercises.
5. Export only allowlisted aggregate counters with
   [metrics-export.md](metrics-export.md). Keep raw broker data and diagnostic
   snapshots local.
6. Gather redacted feedback with [exit-survey.md](exit-survey.md). Review
   failures, limitations, and product decisions with the team, then file generic
   implementation work as separate issues.

## Data handling

The broker sends no background telemetry. Internal reports should omit source,
diffs, task text, secrets, usernames, absolute paths, branch names, and private
repository identity. Do not publish internal metrics as evidence of independent
adoption or product-market fit.

The [protocol](../guides/internal-dogfooding.md) defines observation windows,
measures, recovery exercises, and reporting limits. The
[metrics export](metrics-export.md) describes the local allowlist.
