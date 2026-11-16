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
paths are anchored to the repository's main checkout. See the trust model
below before relying on repository entries.

```toml
schema = 1

[[servers]]
name = "example-server"
mode = "shared"                 # "direct" (default): stateful, one per agent
transport = "stdio"             # "http" is accepted but not served yet
argv = ["pnpm", "exec", "example-server", "--stdio"]
cwd = "/srv/example"            # host file: absolute paths only
env_file = ".env.staging"       # relative to cwd
env_allow = ["API_URL"]         # more names copied from the agent
source = "/srv/example/src"     # content identity is part of the key
flavour = "staging"

[[servers]]
name = "example-browser"
argv = ["npx", "-y", "example-browser@1.4.2", "--isolated"]
```

- `argv` is visible to every local user in `ps`. Keep credentials in
  `env_file`.
- `env_file` is data, not a script: `NAME=VALUE` lines, `#` comments, an
  optional `export ` prefix, single quotes (literal) and double quotes (`\n`,
  `\t`, `\"`, `\\`). Nothing is expanded; `$HOME` stays `$HOME`.
- The server's environment is a default set taken from the agent (`PATH`,
  `HOME`, `USER`, `LOGNAME`, `SHELL`, `LANG`, `LC_ALL` and every other `LC_*`,
  `TMPDIR`, `TZ` and every `XDG_*`), then the `env_allow` names, then the env
  file's assignments, which win. `env_inherit = "none"` drops the default set.
- Every path must name a stable copy. An entry whose `cwd`, `source`,
  `env_file` or path-like `argv` element (absolute, relative with a `/`, or
  the value of `--option=/path`) lies inside a broker session worktree is
  refused with `session_worktree_path`: sessions are cleaned up when they
  finish, and the entry would break with them.

## Trust model for repository entries

A repository file arrives with a clone or a branch, so it is not the owner's
word, and an entry runs with the owner's environment and credentials:

- A host entry always wins. A repository entry with the same name as a host
  entry is ignored; `connect` says so on stderr and `status` counts it
  (`shadowed`, `shadowed_repository_entries`).
- A repository entry may only name paths inside its repository (`cwd`,
  `source`, `env_file` and path-like `argv` elements, symlinks resolved);
  otherwise it is refused with `outside_repository`. A bare program name in
  `argv[0]` is looked up on `PATH`.
- A repository entry runs only after the owner has reviewed and trusted that
  exact file:

  ```text
  aethyme mcp trust [--repo <path>] [--json]
  ```

  prints what the file declares (each entry's `argv`, `cwd`, `env_file`,
  `env_inherit` and `env_allow`) and records the repository identity and the
  SHA-256 of the file in `<host state>/mcp/trusted-repositories.json`. Any
  change to the file, on any branch, is a different digest: `connect` refuses
  the entry (`untrusted_repository_entry`, exit 3, naming `aethyme mcp trust`)
  and runs `--fallback` when one was given.

## Resolving offline

```text
aethyme mcp resolve example-server [--json]
```

prints the declared argv, the direct argv, the executable's identity (path,
size, modification time and, for scripts, the `#!` interpreter), the chosen
copy and the key. Resolution never runs anything and never downloads:

| Declared | Looked up |
| --- | --- |
| `pnpm exec X ...` | `node_modules/.bin/X` from `cwd` up to its project root |
| `npm exec P[@v] -- ...` | `node_modules/P` from `cwd` up to its project root and its `package.json` `bin`; for a pinned version (or `allow_cached = true`) npm's `_npx` cache |
| `npx [-y] P[@v] ...` | the same as `npm exec` |
| `env cmd ...` | `cmd`, resolved the same way |
| `yarn`, `bunx`, `bun x`, `bun run`, `pnpx`, `pnpm dlx`, `env NAME=VALUE ...` | not resolved and never run (`unsupported_launcher_form`) |
| anything else | the program itself, on the agent's `PATH` when bare |

The project root is the nearest directory, from `cwd` up, that is a
repository root or holds a lockfile or workspace file (`pnpm-workspace.yaml`,
`pnpm-lock.yaml`, `package-lock.json`, `npm-shrinkwrap.json`, `yarn.lock`,
`bun.lock`, `bun.lockb`); without one, only `cwd` itself. A package's `bin`
must stay inside the package (`bin_outside_package`).

An exact version must match the installed one, and a range must contain it.
An unpinned request (`@latest` or no version) with no local install is not
resolved (`unpinned`) unless the entry sets `allow_cached = true`: npm's cache
may hold any old copy. With `allow_cached`, `@latest` takes the newest cached
copy and says that it was not checked against the registry.

A script starting with `#!/usr/bin/env node` is started as
`<node from PATH> <script>`. A `#!/bin/sh` shim of exactly the shape pnpm
writes into `node_modules/.bin` is read to its target and started as
`<node> <target>` (node beside the shim, else from the agent's `PATH` at
resolve time), with the `NODE_PATH` the shim would export; any other shim is
kept, since it `exec`s its target.

When nothing installed matches, `resolve` reports `not_installed` (or
`unpinned`), keeps the declared argv and exits 6. Launcher options it does not
understand (`npx -p`, dist-tags such as `@next`) are reported as
`unsupported_launcher_form` or `unsupported_version` rather than guessed.

The guarantee that no package manager runs covers the launchers in the table.
A command that hides a launcher (`sh -c "npx ..."`, a wrapper script) is
treated as the program it names and is executed.

The last stable-copy resolution of each entry is remembered under
`<host state>/mcp/resolved/`; when the executable changes, `resolve` and
`connect` say which part changed (`argv`, `path`, `content`, `shebang`,
`interpreter`).

## Connecting

```text
aethyme mcp connect example-server --fallback -- pnpm exec example-server --stdio
```

`connect` validates only the requested entry, then replaces its own process
with the resolved server (`exec`), with the entry's `cwd` and environment.
The agent's stdin and stdout are the server's, and no extra process stays
alive. It writes one line to stderr:

```text
aethyme mcp: using stable copy, key 3f2a9c41d07b
```

Before the exec it checks what can still fail while a fallback is possible:
the program and its interpreter exist and are executable, and a kept shell
shim's `node` is on the server's `PATH` (`preflight_failed`).

Any failure before the exec (a usage error, a missing, refused or untrusted
entry, a failed resolution or pre-flight check, even an internal panic)
explains on stderr and exits non-zero (2 for usage, 3 for a missing, refused
or untrusted entry, 6 for a resolution or start failure). With
`--fallback -- <command...>` it runs that command instead, in
`--fallback-cwd` when given, with the agent's own environment, so the agent
is never blocked by `aethyme mcp`.

In `mode = "direct"` a failed identity (a source tree over the limits, a Git
error) never blocks the start: the plan notes it on stderr, leaves it out of
the key and marks the key incomplete.

`aethyme mcp config print --agent codex|claude [--name <name>]` prints the
configuration snippet in that agent's format, with the entry's declared argv
as the fallback. It only prints, and skips untrusted repository entries. Keep
any `env` settings the server already has in that configuration: they reach
the fallback. For an entry with an `env_file`, replace the printed fallback
with the command you use today if that one loads the file.

## Testing a branch of a server

An agent working on a server in a broker session worktree must get its own
copy of that server, not the stable one. `connect` (and `resolve`) run from
inside a session worktree of the repository holding the entry's stable copy
choose:

| Situation | Copy | Reason code |
| --- | --- | --- |
| not in a session worktree | stable | `not_in_session_worktree` |
| worktree's `source` and lockfiles identical to the stable ones | stable | `source_identical` |
| worktree's `source` differs | worktree | `source_differs` |
| only the lockfiles at the checkout root differ | worktree | `lockfile_differs` |
| `--from-worktree` | worktree | `requested` |
| entry declares no `source` and lockfiles are identical | stable | `no_source_declared` |
| the worktree has no copy of `source` | stable | `worktree_copy_missing` |
| the trees could not be compared (direct mode) | stable | `source_identity_failed` |

The worktree copy maps every path under the stable checkout (`cwd`,
`env_file`, `source`, absolute `argv` elements) onto the worktree, and then
resolves the launcher chain there, so the worktree's own `node_modules` is
used. A worktree whose dependencies are not installed reports `not_installed`
(and runs the fallback when one was given).

`source` identity is content-based. Inside a Git checkout it is the index
entries under `source` (blob ids, through a literal pathspec) plus the content
of every modified or untracked, not ignored, file; elsewhere, every file's
path and content (skipping `.git` and `node_modules`). Both stop at 20,000
files or 256 MiB, so a `source` inside a large outer repository fails fast.
Git identities are cached for 15 seconds under
`<host state>/mcp/identity-cache/`, keyed by the checkout's HEAD, its index
file's size and mtime and the source directory's mtime (read without running
Git); an edit that changes none of those within the 15 seconds is not seen.

The key hashes what makes two servers interchangeable: the resolved argv and
working directory, written relative to the chosen copy; the identity of
executables outside that copy (an upgraded interpreter is another server);
the content of the entry script inside it (ignored build output included),
the resolved package's name and version, and the lockfiles at the copy root;
the `source` identity; the flavour, mode, transport and `env_inherit`; for
repository entries, the repository; and the values the server receives
beyond the default set (the `env_allow` values and the env file). Those
values, and the env file's identity shown by `resolve`, enter only as
HMAC-SHA256 under a per-host secret (`<host state>/mcp/key-secret`, mode
0600, created on first use), so nothing derived from a credential can be
recovered from a key, `events.jsonl` or `--json`. Sessions whose trees are
identical share a key, and two branches get two keys. The shared hub will
run one server per key.

## Status and telemetry

`aethyme mcp status [--json]` lists the files read, the repository file's
trust state, every entry with its plan (copy, resolution, identity, key),
refused and shadowed entries, and per entry counters from
`<host state>/mcp/events.jsonl`: outcomes (`exec`, `exec_failed`, `fallback`,
`error`, `resolved`, `unresolved`), failure reasons, shadowed repository
entries and resolve time (p50 and maximum). Each `connect` and `resolve`
appends one line with its phase timings (`config_load`, `copy_choice`,
`env_file`, `source_identity` with `source_identity_cache_hits`, `resolve`,
`key`, `total`); a failed exec appends one `exec_failed` line that corrects
the `exec` before it. The file keeps the newest 2,000 lines once it passes
1 MiB. Neither file holds environment values.
