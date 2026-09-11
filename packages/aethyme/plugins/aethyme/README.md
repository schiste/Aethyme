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
brew install schiste/tap/aethyme
# or, from a checkout:
cargo install --path packages/aethyme/rust/crates/aethyme-cli
cargo install --path packages/aethyme/rust/crates/aethyme-engine
```

Then let the CLI install its own hooks, on every agent surface it finds:

```bash
aethyme plugin install
```

This is the recommended path, and not only for convenience. The plugin ships
files and the CLI ships logic, so the two can be installed separately and end
up mismatched — which produces no error, just a plugin that quietly does
nothing. Installing through the CLI means the binary that answers `aethyme
hook` is the one registering the hooks, and that mismatch cannot arise.

Pass `--surface codex` or `--surface claude` to pick one, `--source
/path/to/Aethyme` to install from a checkout, and `--dry-run` to see the
commands first. `aethyme plugin remove` undoes it.

To check an install that already exists — including one done by hand:

```bash
aethyme plugin status
```

It reports the plugin version on each surface and, separately, whether the
`aethyme` on `PATH` actually serves `hook`. Those are different binaries and
the second is the one the hooks reach, so a `brew`-installed CLI shadowed by a
`cargo`-installed one (or the reverse) shows up here rather than as silence.
It also reports the engine pair — see [Split pair](#split-pair) below. It exits
nonzero when the plugin is installed but inert, or when the pair is split.

The manual equivalents are below.

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

## What `SessionStart` says about the installation

Two things about this machine's Aethyme are worth a sentence at a session
start, and are invisible everywhere else.

### Split pair

`aethyme` and `aethyme-engine-cli` are one product in two binaries and must
come from one tree. On a machine that installs them with `cargo install
--path`, there is one `~/.cargo/bin` and many worktrees, so two sessions
installing from different branches on the same afternoon leave a router from
one beside an engine from the other. Both then report a plausible version, and
the pair can fail in ways neither version explains.

The two builds usually share a version number and differ only in the
`-N-g<sha>` suffix `git describe` adds, so the comparison includes it:

```
aethyme               0.7.16 (v0.7.16-8-gf74bf96b)
aethyme-engine-cli    0.7.16 (v0.7.16-6-g080d3c8a)   # same version, different commit
```

Reinstalling both in one go is the fix, and the notice says so. Nothing is
installed, upgraded, or repaired on your behalf.

### A newer release

Read from a cached copy of the release manifest, never from the network. The
notice names `aethyme update`'s recommended command for this installation and
stops there; running it stays a decision a person makes.

`SessionStart` runs at a turn boundary, so it must not wait on a network. It
reads the cache and nothing else. On a cold or expired entry it spawns a
detached `aethyme update check --refresh`, says nothing this time, and finds
the answer on disk — free to read — at the next session. A machine with no
network is throttled rather than retrying forever, since a failed fetch leaves
no entry behind to expire.

This does not weaken `aethyme update`'s rule that nothing installs in the
background: what runs detached downloads a manifest and prints a verdict, and
nothing it does can change a binary.

| Variable | Effect |
| --- | --- |
| `AETHYME_UPDATE_CHECK=off` | Silence both notices and the background refresh. |
| `AETHYME_UPDATE_CACHE_TTL_SECONDS` | How long a cached manifest counts as current. Default 6 hours; `0` disables the cache, and with it the fallback to an expired copy when the network is down. |

`aethyme update check --refresh` re-asks immediately, whatever the cache holds.

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
