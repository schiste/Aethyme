# Cross-revision accepted baseline regressions

The [ledger](ledger.json) inventories five focused command attempts and two
deliberate guard removals, with their original metadata, logs and numeric status
files. No test was rerun to prepare this record.

The latest focused command passed all three named tests. The earlier compiler
failure and three failed focused commands remain visible. These overlapping
executions are separate from the actual twenty-change native FX17 sequence.

Both guard removals failed their intended named assertion with exit 101. Their
unique backups and restored source hashes match the reviewed source. The
metadata names the parent HEAD and records uncommitted changes; the latest
passing patch digest equals the complete patch from that parent to `9677dc7f`.

Independent review found no additional source blocker within its requested
scope. Strict CI Clippy rejected the eight-field tuple in that commit. The
private named-row cleanup and subsequent checks require their own evidence.

The original Rust test logs contain trailing spaces on interrupted test-status
lines and one blank line at EOF. Those raw artifacts are preserved byte-for-byte;
the whitespace checker reports them. Documentation and JSON are checked separately.
