# Upgrading to Aethyme v0.7.17

Last Updated: 2026-09-10

v0.7.17 makes the agent-surface plugin installable by the CLI itself, and makes
the one way that install can silently do nothing say so out loud. No broker
schema change, no change to the generated agent policy.

## What is new

`aethyme plugin` installs the plugin on every agent surface it finds:

```bash
aethyme plugin install            # Codex and Claude Code, whichever are present
aethyme plugin install --surface codex --dry-run
aethyme plugin status
aethyme plugin remove
```

This replaces four hand-run commands across two package managers, but the
reason to prefer it is not brevity.

The plugin ships files -- four hook registrations and a shim -- and the CLI
ships every rule those hooks apply. That split is deliberate: it means an
installed plugin never has to match an installed CLI version. It also means the
two are installed by different commands at different times, and can end up
mismatched. When they do, the failure is invisible. The shim is required to
print nothing but a hook envelope on stdout, so a CLI that cannot serve
`aethyme hook` produces no error, no context, and no clue -- the plugin simply
appears not to work.

Installing through the CLI closes that hole structurally: the binary that
answers `hook` is the one registering the hooks, so the two cannot disagree.

## Diagnosing an install you already have

`aethyme plugin status` reports both halves, and deliberately inspects the
`aethyme` on `PATH` rather than itself -- that is the binary the shim will
actually reach, and it is not necessarily the one you just ran:

```
Hook floor:  aethyme >= 0.7.17
On PATH:     aethyme 0.7.16 (v0.7.16-8-gf74bf96b)  (/Users/me/.cargo/bin/aethyme)
             serves `hook`: yes
This binary: aethyme 0.7.17

Codex:       plugin 0.1.1 installed
Claude Code: plugin not installed
```

It exits nonzero when a plugin is installed in front of a CLI that cannot serve
it. `--json` reports the same facts for scripts.

Note what the sample above shows: a version *below* the stated floor that
serves `hook` anyway. The floor names the first released version containing the
subcommand, but a development build tagged from the release before it has the
subcommand too. So `status` asks the binary instead of comparing strings -- it
runs `aethyme hook`, which a capable CLI answers with exit 0 without touching
the broker. The version is reported, and supplies the remedy, but does not cast
the vote. `--json` keeps both under `answers_hook` and `version_meets_floor` so
a disagreement stays visible.

## Compatibility

No database migration. Schema stays at 32, so a v0.7.16 binary and a v0.7.17
binary read the same broker state and rollback is unrestricted.

`aethyme plugin` is a new subcommand; nothing existing changes behaviour. The
hook shim's fallback telemetry now records the CLI's exit status alongside the
event name, which is additive to a detail string no consumer parses.

The plugin's own version is unchanged at 0.1.1: its shipped files are the same
hooks wired to the same entry point.

## Before upgrading

Account for every copy of the pair on this machine. A tap install and a
`cargo install` shadow each other, and only the one earliest on `PATH` runs:

```bash
which -a aethyme aethyme-engine-cli
```

This matters more than usual here. The `PATH` copy is what the plugin's hooks
invoke, so upgrading a copy that is *not* first on `PATH` leaves the plugin
exactly as inert as before while `aethyme --version` reports success. That is
the mismatch this release exists to surface, and `aethyme plugin status` prints
the resolved path precisely so you can see which copy won.

## Install or update

The router and its engine sibling are one release unit -- never install one
without the other.

```bash
brew update && brew upgrade aethyme
# or, from a checkout
cargo install --path packages/aethyme/rust/crates/aethyme-cli --force
cargo install --path packages/aethyme/rust/crates/aethyme-engine --force
```

## Migrate and verify

Nothing migrates. Confirm the version, then confirm the plugin and the CLI
agree:

```bash
aethyme --version
aethyme plugin status
```

If the plugin is not installed yet:

```bash
aethyme plugin install
```

Codex asks you to trust each hook on its first fire. Trust is recorded in
`~/.codex/config.toml` under `[hooks.state]`; until you grant it, the hooks are
registered but do not run.

## Rollback

Unconstrained -- there is no schema change to undo. Reinstall the previous pair:

```bash
brew install schiste/tap/aethyme@0.7.16   # or cargo install --version 0.7.16
```

The plugin can stay installed across a rollback. Its hooks will find a CLI that
still serves `aethyme hook`, since 0.7.16 introduced it; only `aethyme plugin`
itself disappears. To remove the plugin as well, do it before rolling back,
while the subcommand still exists:

```bash
aethyme plugin remove
```

## Known issues

`aethyme plugin status` and `install` shell out to `codex` and `claude`. A
surface whose CLI is absent is reported as absent and skipped, not treated as a
failure -- but a surface whose CLI is present and hangs will hang the command,
because the probe has no timeout. This is the same exposure the shim already
has on every hook event.

`aethyme plugin install` writes to `~/.codex` and `~/.claude`, which is
machine-global rather than repository-local. It is deliberately a command you
run, never a side effect of `aethyme enhance deploy`.

## Added

- `aethyme plugin install|status|remove`, installing the agent-surface plugin
  from the CLI that serves its hooks.
