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

`mode = "direct"` entries get one server per agent. `mode = "shared"` stdio
entries go through a shared hub: one server per key for the whole host,
multiplexed between agents (see "Shared servers" below).

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
  aethyme mcp trust [--repo <path>] [--yes --digest <sha256>] [--json]
  ```

  prints what the file declares (each entry's `argv`, `cwd`, `env_file`,
  `env_inherit` and `env_allow`) and its SHA-256 first, and records nothing
  until the owner confirms: by typing `yes` on a terminal, or with
  `--yes --digest <sha256>` matching the digest just printed (so a
  non-interactive caller must have read the output). It then records the
  repository identity, the digest and how it was approved (`terminal` or
  `digest`) in `<host state>/mcp/trusted-repositories.json`, under a lock so
  concurrent runs lose nothing. Any
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
Only the stable copy's Git identity is cached, and only for direct entries:
15 seconds under `<host state>/mcp/identity-cache/`, keyed by the checkout's
HEAD, its index file's size and mtime and the source directory's mtime (read
without running Git). The worktree side is never cached, because an unstaged
edit changes none of those markers, and shared entries never use the cache.

The key hashes what makes two servers interchangeable: the resolved argv and
working directory, written relative to the chosen copy; the identity of
executables outside that copy (an upgraded interpreter is another server);
the content of the entry script inside it (ignored build output included)
and, for shared entries, every file in the build output directory holding it
when `source` does not cover it (capped like the file walk; over the cap the
agent starts a private server); the resolved package's name and version, and
the lockfiles at the copy root; the `source` identity; the flavour, mode,
transport and `env_inherit`; for repository entries, the repository; and the
values the server receives: for direct entries the `env_allow` values and
the env file, for shared entries every variable passed to the server (names
and values; `HOME`, `XDG_*`, `PATH` and `TMPDIR` select credential and
configuration files). Those
values, and the env file's identity shown by `resolve`, enter only as
HMAC-SHA256 under a per-host secret (`<host state>/mcp/key-secret`, mode
0600, created on first use as a complete temporary file hard-linked into
place, so a crash never leaves a short secret and a racing process uses the
winner's), so nothing derived from a credential can be recovered from a key,
`events.jsonl` or `--json`. A short or unreadable secret is reported on
stderr and counted (`key_secret_unavailable`); the key is then private to
that process and a shared entry starts a private server. Sessions whose trees are
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

## Shared servers

For a `mode = "shared"`, `transport = "stdio"` entry, `connect` attaches the
agent to the host's hub instead of starting its own server:

- The hub (`aethyme mcp hub serve`, internal) is started by the first
  `connect` that needs it, never at boot, with the broker service's
  discipline: a private 0700 directory `<host state>/mcp/hub/`, a 0600
  socket, a permanent singleton lock and a separate startup lock, its own
  executable by absolute path in its own process group. Socket and lock names
  carry the build identity: a hub from a newer build takes new clients while
  an older one drains and exits, and a live hub is never killed. Every
  connection is checked to come from the same user, and the hub never logs
  request or response bodies.
- `connect` stays as a thin stdio-to-socket bridge. If no hub answers within
  2 seconds, the key is unhealthy or unshareable, the build differs, or the
  key could not be computed completely, `connect` execs a private server
  exactly as for a direct entry. Once the agent's `initialize` has been read
  it cannot be given back, so a later refusal (another protocol version, an
  upstream that wants client features, no answer within 30 seconds) runs the
  private server as a child and replays that message; only then does a
  `connect` process remain.
- The hub runs one upstream per key, in its own process group, registered
  as a host resource lease held by the hub. It performs `initialize` once and
  answers every client's `initialize` from the cached result (the clients'
  `notifications/initialized` are swallowed). Request ids and progress tokens
  are rewritten to hub-unique numbers and mapped back per client, keeping
  their type; `notifications/cancelled` is translated; server notifications
  (`*/list_changed`, logging) reach every client of that key. Server-initiated
  requests (sampling, roots, elicitation) are routed only when exactly one
  client request is in flight, and answered with a JSON-RPC error otherwise;
  an upstream whose initialize result advertises `sampling`, `roots` or
  `elicitation` is not shared. A client that disconnects has its requests
  cancelled upstream and their late replies dropped. Each client has a
  bounded queue (256 messages) and each upstream a cap of 256 requests in
  flight; overflow closes that client's connection and never blocks the
  upstream.
- When an upstream crashes, its in-flight requests fail with a retryable
  JSON-RPC error (`-32000`, `data.retryable = true`); nothing is replayed. It
  restarts with exponential backoff from 1 to 60 seconds, is initialized
  again, and announces `notifications/tools/list_changed`. Five crashes
  within five minutes mark the key unhealthy for ten minutes; its clients are
  disconnected and new ones start private servers.
- An upstream stops ten minutes after its last client leaves; the hub exits
  a minute after it has no upstream. Nothing is pre-warmed, and the hub
  sleeps until its next deadline instead of polling.
- Upstreams receive the entry's environment (the env file included); the
  agent's environment is not passed to the hub otherwise. Two entries with
  different env files, flavours or sources have different keys and never
  share.

`aethyme mcp status --json` adds the hub: whether it runs, its pid, and per
key the upstream (pid, clients, requests in flight) and counters (clients
connected and disconnected, requests, errors, cancellations, crashes,
restarts, queue overflows, fallbacks by reason, and the upstream's resident
memory, sampled at most once a minute). The hub records upstream starts,
stops, crashes, unhealthy keys and overflows in `events.jsonl`, never message
bodies.
