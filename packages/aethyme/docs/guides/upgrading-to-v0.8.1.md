# Upgrading to Aethyme v0.8.1

Last Updated: 2026-09-23

v0.8.1 fixes two regressions that v0.8.0 introduced. Both were found in real
use within hours of the release.

## What is new

### A repeated `git` or `gh` is refused plainly

`broker git -- git add x` runs `git git add x`. In v0.8.0 this failed as an
unrecognized command, and the refusal told the caller to declare
`--effect write`. Agents did. The command then failed, and the failed write
became an unknown outcome that write-blocked the whole repository until
someone reconciled it.

v0.8.1 refuses a leading `git` or `gh` after `--` before classifying the
command, and says what to fix:

```
the broker already runs `git`; drop the leading `git` after `--`
```

The unrecognized-command refusal now says to check the command name first.
It says to declare `--effect write` or `--effect destructive` only if the
command really is intentional.

### Host failures exit 6, not 4

A gate that could not run on the host is classed `resource_contention` (for
example low disk, which the gates refuse below 8 GiB free) or `environment`
(for example a missing tool). Such a gate never judged the code. In v0.8.0 the
submission still exited 4 ("verification failed"), and status advised
"commit a fix".

In v0.8.1:

- The submission exits **6** and says the code was not judged.
- `broker status` and `broker finish` advise freeing the resource and
  resubmitting without changing code.
- A rejection with any real failing gate still exits 4.
- A graph-integrity rejection still exits 4.

## Compatibility

**No schema migration.** The broker database stays at schema 42. v0.8.1,
v0.8.0 and v0.7.25 binaries can open the same database.

Install the CLI and engine pair together. Scripts that treated exit 4 as
"gate failed" now see 6 for host failures. Retry those after freeing the
resource instead of changing code.

## Before upgrading

- Check for in-flight submissions: `aethyme broker status --json`.

## Install or update

```
cargo install --locked --path packages/aethyme/rust/crates/aethyme-cli
cargo install --locked --path packages/aethyme/rust/crates/aethyme-engine
```

## Migrate and verify

Nothing to migrate.

```
aethyme --version          # 0.8.1; build_commit matches the release tag
aethyme plugin status      # engine pair: matched
```

## Known issues

- A gate's cached verdict can record a failure that was caused by the
  environment. Tests that run nested gates fail when disk runs low mid-run,
  and the result is cached against the tree. Resubmit with `--no-cache` after
  freeing space.
- In a busy repository, `broker gc plan` can take minutes, and any activity
  before `gc apply` makes the plan stale, so apply refuses.
- In a repository whose CLAUDE.md or AGENTS.md is customized, no command
  refreshes the generated guidance. `upgrade` reports the deployment as current
  and `enhance deploy` refuses customized files.
- `aethyme enhance verify` prints "Verification failed" but exits 0.

## Rollback

Unrestricted. Reinstall v0.8.0. It reads the same schema-42 database, and the
exit codes and refusal text revert with the binary.
