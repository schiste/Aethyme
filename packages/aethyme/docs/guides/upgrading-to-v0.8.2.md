# Upgrading to Aethyme v0.8.2

Last Updated: 2026-09-24

v0.8.2 hardens the broker: phase 2 of the recovery plan. One change needs
operator action: repositories whose gate commands have never run on this
machine now need `aethyme broker trust` once.

## What is new

### Gate commands run only once trusted

A repository's `.aethyme/gates.toml` and `prepare.toml` can make the broker run
arbitrary shell when an agent submits, so a freshly cloned repository could run
its commands on your machine unasked. The broker now runs a repository's gate
and prepare commands only after you trust that exact policy:

```
aethyme broker trust --repo <path>     # shows the commands, then records trust
aethyme broker trust status --json      # read-only
```

- **Existing repositories keep working.** A repository with gate history on
  this machine is trusted automatically the first time v0.8.2 sees it, and the
  broker records a `gate.policy_trust_grandfathered` event.
- **New repositories, and changed policies, need `trust`.** Until then, gates,
  submit, the pre-commit hook and prepare refuse with exit 3 and print the
  command.
- **Agents cannot trust.** `trust` refuses without an interactive terminal.

### One place to see what blocks a repository

```
aethyme broker blockers --json
aethyme broker unblock <id> [--outcome succeeded|failed] [--reason ...] [--confirm <gen>]
```

Blocker ids share one namespace: `op:`, `hostop:`, `resource:`, `lease:`,
`gatecache:`, `pidfile:` and `action:`. Each entry names the exact command that
clears it. `unblock` refuses anything that could lose work or that needs an
outcome decision, and names the flag it requires. A cached failing gate verdict
can now be invalidated this way, instead of with a hand-written SQL delete.

### Stricter flags

A flag the subcommand does not read is now an error (exit 2) that says where
the flag applies. Before, it was silently ignored.

`broker cleanup <id> --dry-run` used to ignore `--dry-run` and clean the
session. It now refuses the flag.

### Other changes

- **Schema compatibility across versions.** Older binaries from this release
  on can open a database that a newer binary migrated additively (the
  `min_compatible_schema` marker).
- **Review lane.** Fork PRs are skipped by default (`[review.trigger]
  include_forks = true` to opt in). Reviewers run without push credentials.
- **Git timeouts.** Git subprocesses time out after 10 minutes (exit 6).
- **Process safety.** A gate's process group is signalled only after checking
  it is still the same process.
- **Safer writes and journals.** `enhance deploy` refuses symlinked targets,
  and journals redact credential headers.

## Compatibility

**No schema migration.** The broker database stays at schema 42.

- Install the CLI and engine pair together.
- Scripts that pass flags a subcommand ignored now get exit 2. Remove those
  flags.
- CI that runs `aethyme broker gates run` on a fresh runner has no trust
  record. Trust the policy there explicitly: this repository sets
  `AETHYME_TRUST_NONINTERACTIVE_FOR_TESTS=1` on that step. That variable is
  meant only for tests and for CI that runs its own repository's gates.

## Before upgrading

- Check for in-flight submissions: `aethyme broker status --json`.
- For each repository whose gates have never run on this machine, be ready to
  run `aethyme broker trust --repo <path>` from a terminal.

## Install or update

```
cargo install --locked --path packages/aethyme/rust/crates/aethyme-cli
cargo install --locked --path packages/aethyme/rust/crates/aethyme-engine
```

## Migrate and verify

```
aethyme --version                  # 0.8.2; build_commit matches the release tag
aethyme plugin status              # engine pair: matched
aethyme broker trust status        # per repository
aethyme broker blockers            # what, if anything, blocks this repository
```

## Known issues

- The trust test escape (`AETHYME_TRUST_NONINTERACTIVE_FOR_TESTS`) also skips
  the terminal check when set outside tests.
- Orphaned gate containers (#287) are not yet listed as blockers.
- In a busy repository, `broker gc plan` can go stale before `gc apply`.
- In a repository with customized CLAUDE.md or AGENTS.md, no command refreshes
  the generated guidance.

## Rollback

Unrestricted. Reinstall v0.8.1; it reads the same schema-42 database. Trust
records under host state are ignored by 0.8.1.
