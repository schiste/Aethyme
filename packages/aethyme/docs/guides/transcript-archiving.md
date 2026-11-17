# Archiving closed agent transcripts

Last Updated: 2026-11-17

Agent CLIs keep every session's transcript forever. On a busy host they add
up to tens of GiB, almost none of which is read again once the session is
over. `aethyme transcripts` compresses and moves the transcripts of finished
sessions, and puts any of them back on request. zstd at level 3 shrinks agent
transcripts four to five times.

It is on by default, for Codex and Claude Code, on every host that has not
set `archive` in its settings: with no settings file, or a file without an
`archive` line. An explicit `archive = false` (`aethyme transcripts
disable`) turns it off. While it is off nothing runs: no agent directory is
opened, listed or stat'ed, and only Aethyme's own settings file is read
(once per process).

### First run on a host that never chose

Because the default moves the owner's files, the background worker archives
nothing on such a host until 24 hours after `aethyme broker status` first
shows this one-line notice (advice `transcripts.archiving-default-on`,
severity notice): what will be archived, after how many inactive days, and
how to change the days (`aethyme transcripts enable --after-inactive-days
<n>`) or turn it off (`aethyme transcripts disable`). The time the notice
was first shown is recorded in `<host state>/transcripts/first-run.json`;
showing it again does not restart the wait, and the notice disappears once
the wait is over. Either of these skips the wait:

- `aethyme transcripts enable` (an explicit `archive = true`);
- a manual `aethyme transcripts archive --apply` (recorded in
  `first-run.json` as `acknowledged_by = "archive_apply"`).

The clock starts the first time `broker status` (any form, including an
agent's `status --json`) or `broker start` computes the notice. So that a
person sees it during the wait, it is shown on every surface while the gate
is waiting, always with the start time:

- `aethyme broker status` text (compact, `--summary` and `--full`) and its
  JSON: the advice summary names the start time (`at <RFC 3339 UTC> (in
  about N h)`), so the compact document, which keeps only id, severity,
  summary and commands, carries it; the full form also has the evidence
  `automatic_archiving_from_ms=<ms>`;
- `aethyme broker start` text output: a `Notice (transcripts.archiving-default-on): ...`
  line followed by the commands;
- `aethyme transcripts status`: a `first run:` line with the gate and the
  start time, the notice and the commands. `--json` reports `enabled`,
  `enabled_source` (`default` or `settings`) and `first_run` (`gate`:
  `open`, `notice_pending` or `waiting`; `noticed_at_ms`,
  `acknowledged_at_ms`, `acknowledged_by`, `automatic_archiving_from_ms`,
  `automatic_archiving_from`, `notice`, `commands`). Reading the status
  never starts the wait.

## What qualifies

A transcript is archived only when all of these hold:

- it has been inactive for more than `archive_after_inactive_days`
  (default 3), or for Claude Code `archive_after_inactive_days_claude`
  (default 14). Its last activity is the newest of: the file's modification
  time; the `timestamp` of the newest complete record in its last 64 KiB
  (only the tail is read, and only once the modification time is old
  enough); and the `last_activity_at` of every broker session linked to it.
  Skips are counted as `too_recent`, `recent_record` and
  `recent_session_activity`;
- no process has it open, checked when archiving starts and again just
  before the original is removed (a running Codex keeps its transcript files
  open; a running Claude Code does not, which is why the age rule matters);
- when it is linked to broker sessions in any repository's broker database
  known on this host, every linked session is closed or cleaned and its
  checkout directory no longer exists.

- Claude Code only: no running `claude` process has a working directory
  that maps to the transcript's project directory, and none names its
  session id. Claude Code keeps no file open while a session is idle, so
  without this an idle session's transcript would be split. If the process
  table cannot be read, every Claude transcript is skipped.

A transcript linked to no broker session needs only the inactivity and not-open
(and, for Claude, no-live-process) conditions, and only when every broker
database and live-holder listing was read that pass: if any could not be
read, unlinked transcripts are skipped as `linkage_unreadable`. Anything unreadable or ambiguous is skipped and counted with a
reason (`session_open`, `checkout_present`, `open_by_process`,
`open_check_unavailable`, `linkage_unreadable`, ...).

Links come from the `session.holder_bound` event the broker records when an
agent takes a session: its command usually names the agent session id
(`codex ... resume <id>`). New bindings are also linked through the holder's
open transcript files on Linux. Older events are read lazily from every broker
database the lane can find: configured `repositories`, the host's worktree
registry, and the repository a transcript's recorded `cwd` belongs to.

## Where archives go

- Codex: `<codex home>/archived_sessions/<same name>.jsonl.zst`. Codex reads
  `.jsonl.zst` natively: `codex unarchive <id>` moves it back and
  `codex resume <id>` resumes it (and rewrites it as plain `.jsonl`). A
  `.jsonl.gz` file does not work: `codex unarchive` fails on it. Transcripts
  Codex already archived uncompressed are compressed in place.
- Claude Code is not known to read a compressed transcript, so its archives
  live under Aethyme's host state, `<host state>/transcripts/claude/<project
  directory>/<id>.jsonl.zst`, and come back only through
  `aethyme transcripts restore`.

Every archive is recorded in `<host state>/transcripts/index.sqlite3` with
its original path and SHA-256.

## Settings

```bash
aethyme transcripts enable
aethyme transcripts enable --after-inactive-days 5 --providers codex,claude
aethyme transcripts disable
```

`enable` and `disable` write only `<host state>/transcripts/settings.toml`
(`--after-days` is an alias of `--after-inactive-days`; either writes
`archive_after_inactive_days` and drops an older `archive_after_days`
line):

```toml
[transcripts]
archive = true                 # omitted: on by default (first-run wait applies)
archive_after_inactive_days = 3
archive_after_inactive_days_claude = 14
providers = ["codex", "claude"] # the default
# codex_home = "/absolute/path"     default: $CODEX_HOME, then ~/.codex
# claude_home = "/absolute/path"    default: $CLAUDE_CONFIG_DIR, then ~/.claude
# repositories = ["/absolute/repo"] extra broker databases to read
# bytes_per_second = 33554432       unattended read rate (floor 4 MiB/s)
# free_space_floor_bytes = 21474836480
```

The older names `archive_after_days` and `archive_after_days_claude` are
still read; when both spellings are present the `archive_after_inactive_*`
one wins.

While it is on (and past the first-run wait), the broker's detached background worker runs one archive pass
each time it runs: at background CPU and I/O priority, at most
`bytes_per_second` (never slower than 4 MiB/s), largest transcripts first so
space comes back early, for at most ten minutes per pass. When the archive
volume has less free space than `free_space_floor_bytes` the pass runs
unpaced. Either way a transcript is only archived when its volume has room
for the compressed copy (a third of its size plus 256 MiB), largest that fits
first; when none fits the pass stops with an `insufficient_space` warning. A
destination conflict is recorded once and retried after 1, 2, 4 ... 32 days. Memory pressure does not pause it. One pass runs at a time per host;
the lock is released by the kernel if a run dies.

## Commands

```bash
aethyme transcripts status --json
aethyme transcripts list --json
aethyme transcripts archive --dry-run
aethyme transcripts archive --apply
aethyme transcripts restore 0b9f6c1e-2d3a-4b5c-8d7e-1f2a3b4c5d6e
```

- `status`: settings, archived files, bytes before and after compression,
  bytes an archive pass could reclaim now, skip counts by reason, lifetime
  counters, the last run, linkage health (whether session links could all
  be read now, and for how many passes in a row they could not, by kind of
  issue) and durability (sources kept because a sync failed, `fsync`
  fallbacks, unfinished restores). After three degraded passes in a row,
  `aethyme broker status` shows the advice `transcripts.linkage-degraded`.
- `list`: archived transcripts, plus the ones a pass would archive or skip.
- `archive --dry-run` prints what would move; `archive --apply` does it now.
- `restore <session-id|path>` decompresses back to the original location,
  checks the SHA-256 against the index, then removes the archive. When a
  newer transcript exists at the original path (a session that continued
  after it was archived), the result is the archived content followed by
  that file's lines: written to a temporary file, verified by re-reading it,
  and atomically put in its place, with the newer file kept under a backup
  name until then; bytes appended to it meanwhile are carried over, and a
  backup that cannot be reconciled is kept and reported. Such a merge is
  refused while the session may still be writing (a matching `claude`
  process, or for Codex the file open or a `codex` process naming the
  session; an unreadable process table counts as live). A restore records
  an intent before publishing; if it was interrupted after publishing, the
  next `restore` finds the file already starting with the archive's exact
  bytes and only finishes the cleanup, never merging twice. A Codex archive with no
  index row (for example after the index was lost) is found by its id and
  restored to Codex's dated `sessions/` directory, vouched for by the zstd
  frame checksum.

## Crash safety

A transcript is compressed into a temporary file beside its destination,
flushed to stable storage (`F_FULLFSYNC` on macOS, where `fsync` leaves
data in the drive cache), re-checked (size, mtime and inode unchanged), and
decompressed again to compare its length and SHA-256 with the source. Only a
verified copy is published (a no-clobber link, then a directory sync) and
recorded in the index; any sync failure keeps the source and drops the copy.
The source is removed last, after one more check that it is unchanged and
not open in an open-file listing taken right then; if it changed, is open,
or the listing cannot be taken, the copy is discarded instead.
The original and the archive are therefore never both gone. A run killed
after publishing leaves both; the next run verifies the archive decodes to
exactly the source and finishes the removal. A temporary file left by a
killed run is removed by the next run.

## Confinement and privacy

Transcripts hold secrets, so every file operation is confined:

- Each provider root (the Codex home, Claude Code's `projects/` directory,
  and Aethyme's `<host state>/transcripts/`) must exist, be a real directory
  rather than a symbolic link, and be owned by the current user; otherwise
  that provider is skipped (`unsafe_path`) with a warning.
- Below a root, paths are opened one component at a time without following
  symbolic links, every directory must be on the root's device (no mount
  point is crossed), and an original is only removed by name in its opened
  directory when it is still exactly the file that was compressed.
- Archives and temporary files are created exclusively (never through an
  existing entry or link) with mode 0600; directories the lane creates are
  0700, and a directory an archive is written into is narrowed to 0700.
  The index, run log, lock and settings files are 0600 in a 0700 directory.
- Logs, metrics and warnings carry counts and reasons only: no transcript
  content, and no path outside the provider roots (an unreadable broker
  database or repository is counted, not named).

When an archive is moved away by something else (for example
`codex unarchive`), its index row stops counting at once and is dropped by
the next pass.

## Telemetry

Each pass records phase timings (`transcripts_scan`,
`transcripts_inactivity` (the modification-time and tail-record tests),
`transcripts_linkage` with its parts `transcripts_linkage_cwds`,
`transcripts_linkage_databases`, `transcripts_linkage_backfill` and
`transcripts_linkage_live_holders`, `transcripts_judge`,
`transcripts_open_check`, `transcripts_compress`, `transcripts_verify`,
`transcripts_archive`, `transcripts_total`) into the background worker's
command-metrics row, appends one line to
`<host state>/transcripts/runs.jsonl` (with `enabled_source`,
`first_run_gate`, `record_tails_read`, eligible count and bytes, archived
count, bytes in and out, skips by reason), and adds to lifetime counters
(`examined`, `archived`, `bytes_in`, `bytes_out`, `resumed`,
`verify_failures`, `restored`, `skipped_<reason>`) shown by `status --json`.
