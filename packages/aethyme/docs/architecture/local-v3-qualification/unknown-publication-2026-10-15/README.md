# Actual ambiguous publication control

The [ledger](ledger.json) inventories all three attempts and their original
metadata, logs and status files. Each artifact has a byte count and SHA256.
Attempt 003 also retains its raw structured report. Exact command, tested
source/tree, finite allowances and actual statuses are preserved per attempt.

Two fixture attempts failed with exit101 and receive no ambiguous-write credit.
The corrected attempt passed one explicit native test at `b16af023`: the
receiver ref moved before the managed push timed out; the service held and
refused replay, with zero private acceptance receipts, qualifications or
completions. Both owned hook processes exited. Retained roots and conservative
charges stayed visible.

This is one finite schedule. It supplies no repeated external-drift or FX17
reliability claim. Private temporary-root archiving is recorded separately;
these repository artifacts preserve the original observations. No experiment
was rerun to prepare this record.
