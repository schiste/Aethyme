# MCP servers without launcher chains

Last Updated: 2026-11-16

Every agent session starts its own MCP servers over stdio. When a server is
started through a package-manager launcher (`pnpm exec`, `npm exec`, `npx`),
each instance keeps one or two launcher processes alive in front of the
server, and on a busy host those launchers cost about as much memory as the
servers themselves. `aethyme mcp` starts the installed server directly
instead: it resolves the launcher chain offline and replaces itself with the
server, so the agent's process *is* the server.

It is opt-in. Nothing is changed in any agent configuration: the owner
declares entries, checks them with `aethyme mcp resolve`, and pastes the
snippet `aethyme mcp config print` prints.

This release launches one server per agent. A shared hub for
`mode = "shared"` entries comes later; the configuration and keys described
here are already the ones it will use.

## Declaring entries

Host entries live in `<host state>/mcp/services.toml` (the host state
directory is `$AETHYME_HOST_STATE_DIR`, else `$XDG_STATE_HOME/aethyme`, else
the platform default). A repository may add `.aethyme/mcp.toml`; its relative
paths are anchored to the repository's main checkout, and an entry there
shadows a host entry of the same name.

```toml
schema = 1

[[servers]]
name = "example-server"
mode = "shared"                 # "direct" (default): stateful, one per agent
transport = "stdio"             # "http" is accepted but not served yet
argv = ["pnpm", "exec", "example-server", "--stdio"]
cwd = "/srv/example"            # host file: absolute paths only
env_file = ".env.staging"       # relative to cwd
env_allow = ["PATH", "HOME"]    # names copied from the agent's environment
source = "/srv/example/src"     # content identity is part of the key
flavour = "staging"

[[servers]]
name = "example-browser"
argv = ["npx", "-y", "example-browser@latest", "--isolated"]
env_allow = ["PATH", "HOME"]
```

- `argv` is visible to every local user in `ps`. Keep credentials in
  `env_file`.
- `env_file` is data, not a script: `NAME=VALUE` lines, `#` comments, an
  optional `export ` prefix, single quotes (literal) and double quotes (`\n`,
  `\t`, `\"`, `\\`). Nothing is expanded; `$HOME` stays `$HOME`.
- The server's environment is exactly the `env_allow` names taken from the
  agent plus the env file's assignments (which win). `PATH` and `HOME` are
  not passed unless listed.
- Every path must name a stable copy. An entry whose `cwd`, `source`,
  `env_file` or absolute `argv` element lies inside a broker session worktree
  is refused with `session_worktree_path`: sessions are cleaned up when they
  finish, and the entry would break with them.

## Resolving offline

```text
aethyme mcp resolve example-server [--json]
```

prints the declared argv, the direct argv, the executable's identity (path,
size, modification time and, for scripts, the `#!` interpreter), the chosen
copy and the key. Resolution never runs anything and never downloads:

| Declared | Looked up |
| --- | --- |
| `pnpm exec X ...` | `node_modules/.bin/X` in `cwd` and its ancestors |
| `npm exec P[@v] -- ...` | `node_modules/P` in `cwd` and its ancestors, its `package.json` `bin`; then npm's `_npx` cache |
| `npx [-y] P[@v] ...` | the same as `npm exec` |
| anything else | the program itself, on the agent's `PATH` when bare |

An exact version must match the installed one, and a range must contain it.
`@latest` takes the newest installed copy and says that it was not checked
against the registry. A script starting with `#!/usr/bin/env node` is started
as `<node from PATH> <script>`, so the server does not need `PATH` itself; a
`#!/bin/sh` shim (what pnpm installs) is kept, since it `exec`s its target.

When nothing installed matches, `resolve` reports `not_installed`, keeps the
declared argv and exits 6. Launcher options it does not understand
(`npx -p`, `pnpm dlx`, dist-tags such as `@next`) are reported as
`unsupported_launcher_form` or `unsupported_version` rather than guessed.

The last stable-copy resolution of each entry is remembered under
`<host state>/mcp/resolved/`; when the executable changes, `resolve` and
`connect` say which part changed (`argv`, `path`, `content`, `shebang`,
`interpreter`).

## Connecting

```text
aethyme mcp connect example-server --fallback -- pnpm exec example-server --stdio
```

`connect` replaces its own process with the resolved server (`exec`), with the
entry's `cwd` and environment. The agent's stdin and stdout are the server's,
and no extra process stays alive. It writes one line to stderr:

```text
aethyme mcp: using stable copy, key 3f2a9c41d07b
```

When the entry is missing or refused, or resolution fails, `connect` explains
on stderr and exits non-zero (3 for a missing or refused entry, 6 for a
resolution or start failure). With `--fallback -- <command...>` it runs that
command instead, in `--fallback-cwd` when given, with the agent's own
environment, so the agent is never blocked by `aethyme mcp`.

`aethyme mcp config print --agent codex|claude [--name <name>]` prints the
configuration snippet in that agent's format, with the entry's declared argv
as the fallback. It only prints. Keep any `env` settings the server already
has in that configuration: they reach the fallback. For an entry with an
`env_file`, replace the printed fallback with the command you use today if
that one loads the file.

## Testing a branch of a server

An agent working on a server in a broker session worktree must get its own
copy of that server, not the stable one. `connect` (and `resolve`) run from
inside a session worktree of the repository holding the entry's stable copy
choose:

| Situation | Copy | Reason code |
| --- | --- | --- |
| not in a session worktree | stable | `not_in_session_worktree` |
| worktree's `source` identical to the stable one | stable | `source_identical` |
| worktree's `source` differs | worktree | `source_differs` |
| `--from-worktree` | worktree | `requested` |
| entry declares no `source` | stable | `no_source_declared` |
| the worktree has no copy of `source` | stable | `worktree_copy_missing` |

The worktree copy maps every path under the stable checkout (`cwd`,
`env_file`, `source`, absolute `argv` elements) onto the worktree, and then
resolves the launcher chain there, so the worktree's own `node_modules` is
used. A worktree whose dependencies are not installed reports `not_installed`
(and runs the fallback when one was given).

`source` identity is content-based. Inside a Git checkout it is the index
entries under `source` (blob ids) plus the content of every modified or
untracked, not ignored, file; elsewhere, every file's path and content
(skipping `.git` and `node_modules`, up to 20,000 files or 256 MiB). Ignored
build output is not part of it.

The key hashes what makes two servers interchangeable: the resolved argv and
working directory, written relative to the chosen copy; the identity of
executables outside that copy (an upgraded interpreter is another server);
the env file's content; the `source` identity; the `env_allow` names; the
flavour, mode and transport; and, for repository entries, the repository.
Sessions whose source trees are identical therefore share a key, and two
branches get two keys. The shared hub will run one server per key.

## Status and telemetry

`aethyme mcp status [--json]` lists the files read, every entry with its plan
(copy, resolution, identity, key), refused entries with their reason, and per
entry counters from `<host state>/mcp/events.jsonl`: outcomes (`exec`,
`fallback`, `error`, `resolved`, `unresolved`), failure reasons, and resolve
time (p50 and maximum). Each `connect` and `resolve` appends one line with its
phase timings (`config_load`, `copy_choice`, `env_file`, `source_identity`,
`resolve`, `total`); the file keeps the newest 2,000 lines once it passes
1 MiB. Neither file holds environment values.
