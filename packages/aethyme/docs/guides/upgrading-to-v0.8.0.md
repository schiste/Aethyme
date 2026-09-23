# Upgrading to Aethyme v0.8.0

Last Updated: 2026-09-23

v0.8.0 is a correctness and integrity release. It changes exit codes and what
a submission is judged by, hence the minor version. No command or flag was
removed, and the broker database schema is unchanged.

## What is new

### Exit codes name the outcome

`aethyme broker` used to exit 1 for every error, and `broker submit --json`
exited 0 even when it rejected. Codes now distinguish what a caller should do:

| Code | Meaning |
| --- | --- |
| 0 | Success, including a verified submission that promotion did not move |
| 1 | Unclassified failure (unchanged) |
| 2 | Usage error |
| 3 | Refused: a policy, lease, confirmation or state precondition, or a conflicted submission |
| 4 | Verification failed: a gate or graph-integrity check |
| 5 | Outcome unknown: inspect external state and reconcile; never retry blindly |
| 6 | Environment: a missing tool, path or remote base, or host I/O failure |

Scripts that test for non-zero keep working. Scripts that test for exactly `1`
on a refusal or a failed submission need updating.

### Submissions are judged by the base policy

`broker submit` now reads `.aethyme/gates.toml` and the `[graph]` policy from
the integration base the change lands on, not from the submitted tree. A
session can no longer pass by setting a gate to `true` or deleting
`gates.toml`. A session that changes gate or graph policy is still judged by the
old policy. Its change applies to submissions after it lands, and the broker
emits `merge.policy_deferred` so the deferral is visible.

To change gates, land the policy change first, then submit work that depends
on it.

### Stricter operation classification

- An unrecognized git or gh command cannot be declared `--effect read`. Declare
  `--effect write` or `--effect destructive`.
- Inline `-c`/`--config-env` keys that run programs or define aliases
  (`alias.*`, `core.hooksPath`, `core.sshCommand`, `core.fsmonitor`,
  `credential.*`, …) and `--exec-path` are refused for coordinated git.
- `+refspec` pushes, bundled short flags (`-fdx`, `-uf`, `-Df`), `git push -d`,
  `update-ref -d`, forced `send-pack`, `reset --hard/--merge/--keep`,
  `clean -f/-d/-x` and `gh api -XDELETE` are destructive and need
  `--destructive`.
- The pre-push hook checks the operation journal. Exported
  `AETHYME_BROKER_OPERATION_ID`/`AETHYME_BROKER_SESSION_ID` values no longer
  unlock a push to a protected branch unless they name a running, non-read
  operation of that session.

### Explore

Explore's auth-specific ranking layer is removed. It was tuned to one
evaluation playground, which the project's eval rule forbids. Auth questions
rank generically now and may score lower until a content search replaces the
layer. Evaluation results from 2026-07-28 to 2026-09-23 should not be cited.

### Fixes

- `broker adopt` records declared and task-derived scope (#285), and
  `broker start --json` does too. `--claim` is refused where it would have been
  silently discarded.
- The promote commit carries `Contract justification:`, so a justified
  `Contract decision: none` passes the contract gate.

## Compatibility

**No schema migration.** The broker database stays at schema 42, so v0.8.0 and
v0.7.25 binaries can open the same database.

Install the CLI and engine pair together. Agents or scripts that branch on exit
code 1, or that declare `--effect read` for unrecognized commands, need the
changes above.

## Before upgrading

- Check for in-flight submissions: `aethyme broker status --json`.
- If a session's branch edits `.aethyme/gates.toml` and expects its own
  submission to run the new gates, land that policy change separately first.

## Install or update

```
cargo install --locked --path packages/aethyme/rust/crates/aethyme-cli
cargo install --locked --path packages/aethyme/rust/crates/aethyme-engine
```

`--locked` is required: without it the resolver selects a `ra-ap-rustc_lexer`
that fails to compile.

## Migrate and verify

Nothing to migrate.

```
aethyme --version          # 0.8.0; build_commit matches the release tag
aethyme plugin status      # engine pair: matched
aethyme broker status      # opens the database normally
```

## Known issues

- Explore returns weaker answers on auth and token questions until the planned
  content search lands.
- `aethyme enhance verify` prints "Verification failed" but exits 0.
- `aethyme graph materialize --help` runs a materialize instead of printing
  help.

## Rollback

Unrestricted. Reinstall v0.7.25; it reads the same schema-42 database. The
exit codes and the base-tree policy revert with the binary.
