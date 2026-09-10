# Aethyme plugin

Broker coordination delivered at turn boundaries instead of by polling.

Without it, an agent learns about coordination state by *asking* — `aethyme
broker status` in the middle of a turn. Each ask costs a full assistant turn,
and measurement on this repository found that 62% of those calls were made by
sessions that were the only session running at the time: coordination bought
nothing. With the plugin installed, hooks fire at turn boundaries for zero
tokens, and the broker speaks only when something actually changed.

## Install

Requires the `aethyme` CLI on `PATH`:

```bash
cargo install --path packages/aethyme/rust/crates/aethyme-cli
cargo install --path packages/aethyme/rust/crates/aethyme-engine
```

### Codex

```bash
codex plugin marketplace add schiste/Aethyme
codex plugin add aethyme@aethyme
```

Codex asks you to trust each hook on first fire. Trust is recorded in
`~/.codex/config.toml` under `[hooks.state]`, keyed by
`aethyme@aethyme:hooks/hooks.json:<event>:0:0`.

### Claude Code

```bash
/plugin marketplace add schiste/Aethyme
/plugin install aethyme@aethyme
```

### From a local checkout

Either surface accepts a path instead of `owner/repo`, which is the way to test
a change before publishing it:

```bash
codex plugin marketplace add /path/to/Aethyme
codex plugin add aethyme@aethyme
```

### Remove

```bash
codex plugin remove aethyme
codex plugin marketplace remove aethyme
```

## What it installs

`hooks/hooks.json` wires five events to one shim, `hooks/aethyme-hook.sh`:

| Event | Broker meaning |
| --- | --- |
| `SessionStart` | Register the session. |
| `UserPromptSubmit` | Mark the session ACTIVE; deliver coordination deltas. |
| `PreToolUse` | Deny a conflicting write. Never claims a lease. |
| `PostToolUse` | Record liveness; mark the worktree dirty. |
| `Stop` | Mark the session IDLE; release leases only if clean. |

`PermissionRequest` is deliberately not wired: nothing in the design needs it,
and an unused hook is a process spawn per permission prompt for nothing.

## Why the shim is thin

The plugin ships files. The CLI ships logic. If the hook contained the
coordination rules, every plugin version would have to match a CLI version, and
the two are installed by different commands at different times.

So the shim does exactly one thing: pipe the event JSON to a single stable
entry point.

```
aethyme hook <event> --repo <git root>   # event JSON on stdin
```

Everything else in the shim exists to be safe about *not* running:

- **It is installed globally and fires in every repository on the machine.** It
  exits silently unless `.aethyme/` exists at the git root.
- **stdout is parsed as a hook envelope.** Every non-envelope path prints
  nothing at all — an error message in that slot corrupts the session.
- **A hook must not fail.** Missing CLI, missing git, unknown subcommand, older
  CLI without `aethyme hook`: all exit 0.

The last case is the version-skew fallback. A CLI that predates `aethyme hook`
exits nonzero; the shim swallows the output, records the invocation for
liveness, and says nothing to the agent. The plugin stays installable against a
CLI that cannot yet use it.

## What it does not do

It does not re-inject `AGENTS.md` or `CLAUDE.md`. Codex loads those natively,
and injecting them again from a hook is pure duplication — measured at 142M
tokens over 48 hours before it was removed.
