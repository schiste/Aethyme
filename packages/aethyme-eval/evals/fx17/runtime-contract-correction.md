# FX17 pretrial runtime contract correction

This separate pretrial correction records the runtime controls that are actually
verified for the FX17 runner. It does not alter frozen prompts, the six-row
schedule, public fixtures, or private oracle bytes. Trial approval must bind to
this document's SHA256 and the runner's runtime-profile SHA256.

## Pinned client and measured limits

- Client: Codex CLI `0.162.1`, at
  `/Users/christophehenner/.local/lib/node_modules/@openai/codex/node_modules/@openai/codex-darwin-arm64/vendor/aarch64-apple-darwin/bin/codex`.
- Client SHA256:
  `74c6a6d263f40d65f99c27a815275f5b4ed0bcc2baecc396b4cf3255498bdfc0`.
- The local runner makes one CLI invocation per assigned row and does not
  retry a failed row. Provider-side request and stream retries are unknown.
- The frozen `32768` token value is a nominal **output-token telemetry
  threshold**. The CLI configuration reference and pinned binary do not
  establish an enforceable output-token ceiling. The runner compares only
  observed provider output tokens with this threshold. Missing usage remains
  unknown, and input-plus-output totals are not compared with it.
- Model event stdout capture is capped at 8 MiB and stderr capture at 2 MiB.
  Captured bytes are retained only up to those per-stream bounds; an observed
  overage kills the process group and is recorded as `output_limit_exceeded`.
  The last-response file is polled and the process group is stopped when it is
  observed above 4 MiB; this is a kill-on-observation limit, not a filesystem
  quota.
- The child process group has a 180-second execution deadline. After timeout or
  output overflow, the runner allows up to two seconds to close pipes and reap
  the process-group leader. The report distinguishes deadline, output-overflow,
  stream-drain, and reaping outcomes.
- Codex tool branches are disabled in the CLI command, the CLI sandbox is
  read-only, and tool use is checked against returned events. Provider request
  counts, retry counts, and usage fields remain null unless the client reports
  them.

The earlier schedule field `max_tokens_per_attempt: 32768` is retained as
frozen provenance. Under this correction, it must be interpreted as the
nominal output-token threshold above, not as an enforced token limit. No trial
is approved by this correction alone.

## Candidate evaluation isolation

The final-proposal evaluator loads the externally supplied frozen oracle in the
parent process. Its child receives only the candidate script and each exact
literal argv vector. Before the controls harness runs, an independent reviewer
must acknowledge the exact runner, control-harness, and subject-profile hashes.
The final-proposal evaluator then requires a matching passing controls report.
Control evidence records the canaries, exact per-probe profile, argv, exit
status, bounded stdout/stderr bytes and hashes, and observed
filesystem/network/process effects.

All controls and candidate executions remain unpaid local operations. No model,
subject, or control process is run by preparing or statically verifying this
profile.
