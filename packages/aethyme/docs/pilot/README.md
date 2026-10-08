# Aethyme pilot kit

Last Updated: 2026-10-08

A bounded external-adopter study: one week observing the team's existing
workflow, followed by two weeks using Aethyme. Three independent repositories
must complete before the results are treated as evidence. The broker lets
several coding agents work on one repository at once: each agent gets its own
worktree and session, overlapping edits are reported before they collide, and
`submit` verifies the merged tree using the repository's configured promotion
mode. Publication is a separate, authorized action.

## Who it is for

- Teams of 2 to 10 developers.
- Already running two or more coding agents at the same time on one
  repository (Claude Code, Codex, Cursor agents or similar).
- macOS (Apple Silicon or Intel) or x86-64 Linux.

## What we ask of you

1. **Record the baseline before install** using
   [baseline-form.md](baseline-form.md). Observe the team's existing workflow
   for one week; keep task descriptions and repository identity out of the
   shared record.
2. **Install and enroll one repository** using the versioned
   [install guide](install.md). Record the exact Aethyme and policy versions.
3. **Use it for real work for two weeks.** Route agent tasks through the six
   broker verbs in [six-verbs.md](six-verbs.md). You can stop at any time.
4. **Run the post-session interview and survey** after the two-week follow-up.
   Keep notes redacted and record missing answers rather than filling gaps
   from memory; see [external-pilots.md](../guides/external-pilots.md).
5. **Export only reviewed aggregate counters** with the numeric allowlist in
   [metrics-export.md](metrics-export.md). The local diagnostic snapshots from
   `export-metrics.sh` include session ids and timestamps; keep them on the
   participant's machine and do not send them.

## What we offer

- Setup help: a call to install, enroll the repository and review the first
  gates with you.
- Help during the two weeks when something blocks you or needs recovering.

## What we measure

Per team: time from install to the first successful `submit`, overlaps and
conflicts the broker caught before they landed, recovery incidents and the
time spent on them, whether you were still using it after week two, and what
you would cut.

## Privacy

**No source code leaves your machine.** The broker runs locally and sends
nothing to us. Participation, sharing aggregate results, and sharing any quote
are separate choices. Share data only after explicit consent and review:

- the numeric-only `pilot-delta.json` and aggregate baseline form;
- redacted check-in notes and survey answers, if separately approved;
- exact anonymized quotations only with separate permission.

Never share `status --json`, raw broker metrics, the output directory created
by `export-metrics.sh`, broker databases, source, diffs, task text, usernames,
absolute paths, branch names, or private repository identity. The
[external-pilot protocol](../guides/external-pilots.md) gives the full consent
and retention rules. A participant may withdraw at any time.

If you use `export-metrics.sh` for local troubleshooting, its filename label
is local-only; do not use a private repository or company name, and do not
share that snapshot or its filename.

## At the end

Only send the reviewed, consented aggregate artifacts to the pilot owner. If
you want to keep using Aethyme, nothing changes; if not, finish or preserve
open session work and follow the reviewed uninstall and rollback steps in
[install.md](install.md). Do not remove `.aethyme/` or host state as an
uninstall shortcut.
