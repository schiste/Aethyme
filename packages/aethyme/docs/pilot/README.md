# Aethyme pilot

Last Updated: 2026-09-26

A two-week pilot of the Aethyme broker with three teams. The broker lets
several coding agents work on one repository at once: each agent gets its own
worktree and session, overlapping edits are reported before they collide, and
work lands on a local integration branch only after your gates pass on the
merged tree. Nothing is pushed unless you push it.

## Who it is for

- Teams of 2 to 10 developers.
- Already running two or more coding agents at the same time on one
  repository (Claude Code, Codex, Cursor agents or similar).
- macOS (Apple Silicon or Intel) or x86-64 Linux.

## What we ask of you

1. **Install and enroll one repository** in the first two days, following
   [install.md](install.md). Plan on about an hour, with us on a call if you
   like.
2. **Use it for real work for two weeks.** Route agent tasks through the six
   broker verbs in [six-verbs.md](six-verbs.md). You can stop at any time.
3. **A weekly check-in**, 20 minutes, at the end of each week: what worked,
   what blocked you, anything you had to recover from.
4. **The metrics export.** Run [`export-metrics.sh`](export-metrics.sh) once
   a day, by hand or from cron, and send us the output directory at the end.
   It holds counts, session ids and timestamps only; see
   [metrics-export.md](metrics-export.md) for exactly what is kept.
5. **The exit survey**, ten questions, about 15 minutes, once per developer:
   [exit-survey.md](exit-survey.md).

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
nothing to us. The only data you share is what you choose to send:

- the metrics directory, which holds no file paths, branch names, task text,
  commit SHAs, agent identities or source (the script refuses to write a
  snapshot if any field looks like a path, and you can read every file before
  sending it);
- your check-in notes and survey answers.

The repository label in the metrics file names is one you choose.

## At the end

Send the metrics directory and the survey answers. If you want to keep using
Aethyme, nothing changes; if not, `aethyme broker finish` your open sessions
and uninstall with `brew uninstall aethyme` (or delete the installed
binaries). The broker's state lives under `.aethyme/` in the repository and
in Aethyme's host state directory.
