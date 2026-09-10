# Aethyme plugin

Broker coordination delivered at turn boundaries instead of by polling.

Without it, an agent learns about coordination state by *asking* — `aethyme
broker status` in the middle of a turn. Each ask costs a full assistant turn,
and measurement on this repository found that 62% of those calls were made by
sessions that were the only session running at the time: coordination bought
nothing. With the plugin installed, hooks fire at turn boundaries for zero
tokens, and the broker speaks only when something actually changed.

## Install

Requires the `aethyme` CLI on `PATH`. The hooks are inert against a CLI
older than 0.7.17, which is where `aethyme hook` was introduced:

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

`hooks/hooks.json` wires four events to one shim, `hooks/aethyme-hook.sh`:

| Event | Broker meaning |
| --- | --- |
| `SessionStart` | Register the session. |
| `UserPromptSubmit` | Mark the session ACTIVE; deliver coordination deltas. |
| `PreToolUse` | Deny a conflicting write. Never claims a lease. |
| `Stop` | Mark the session idle. |

Two events are deliberately left unwired, for the same reason. `UserPromptSubmit`
and `Stop` already bracket a turn, so `PostToolUse` would spawn a process per
tool call to record liveness the turn boundaries already carry; and nothing in
the coordination model reads `PermissionRequest` at all. An unused hook is not
free — it is a process spawn on the hottest path in the session.

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

The symmetric case is handled on the CLI side: `aethyme hook` treats an event
name it does not recognise as a silent success, so a newer plugin wiring a new
event never breaks an older CLI either.

## What it does not do

It does not re-inject `AGENTS.md` or `CLAUDE.md`. Codex loads those natively,
and injecting them again from a hook is pure duplication — measured at 142M
tokens over 48 hours before it was removed.
