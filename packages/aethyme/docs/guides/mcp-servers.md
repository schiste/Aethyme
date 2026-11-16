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
  non-interactive caller must have read the output; an agent that reads the
  digest can still pass it, so this guards against blind approval, not
  against a determined agent). It then records the
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

- The hub (`aethyme mcp hub serve`, internal) is launched by the first
  `connect` that needs it, never at boot, through `aethyme mcp hub launch`,
  which starts it in its own session and exits at once, so the hub is never
  a child of an agent's process. Its environment is reduced to `PATH`,
  `HOME`, `USER`, `LOGNAME`, `TMPDIR`, the locale, `XDG_*` and the Aethyme
  host-state variables. It follows the broker service's discipline: a
  private 0700 directory `<host state>/mcp/hub/`, a 0600 socket, a permanent
  singleton lock and a separate startup lock, its own executable by absolute
  path. Socket and lock names carry the build identity: a hub from a newer
  build takes new clients while an older one drains and exits, and a live
  hub is never killed. Both ends check that the other is the same user
  before anything (argv, environment) is exchanged; connections are capped
  at 256, and the hub never logs request or response bodies.
- `connect` stays as a thin stdio-to-socket bridge. If no hub answers within
  2 seconds, the key is unhealthy, the build or the entry's argv, cwd or
  environment differ from the upstream already running for that key
  (`spec_mismatch`), the hub is full, the key could not be computed
  completely, or the entry sets `require_client_features = true`, `connect`
  execs a private server exactly as for a direct entry. Once the agent's
  `initialize` has been read it cannot be given back, so a later refusal
  (another protocol version, an initialize that never answers) runs the
  private server as a child and replays that message; only then does a
  `connect` process remain.
- The hub initializes every upstream itself, with `capabilities: {}` and its
  own `clientInfo` (`aethyme-mcp-hub`), whatever the first client declared,
  and answers every client's `initialize` from the cached result (the
  clients' `notifications/initialized` are swallowed). Roots, sampling and
  elicitation are client capabilities the hub never offers: every
  server-initiated request except `ping` is answered with method-not-found
  and counted, and clients' `notifications/roots/list_changed` are dropped.
  A client that declares those capabilities still attaches; it simply never
  receives such requests. An entry whose server cannot work without them sets
  `require_client_features = true` and always runs privately.
- Per-session state never crosses agents. `logging/setLevel` is answered by
  the hub and never forwarded, and the upstream's `notifications/message`
  is dropped on shared keys (a log line may carry another agent's
  arguments). Resource subscriptions are reference-counted: the upstream is
  subscribed on the first agent's `resources/subscribe` and unsubscribed on
  the last agent's `resources/unsubscribe` or disconnect, and
  `notifications/resources/updated` reaches only that URI's subscribers; a
  first subscribe the upstream refuses leaves nobody subscribed. After a
  restart the hub subscribes the new process to every URI still followed; a
  refused renewal sends those agents `notifications/resources/list_changed`
  so they re-read. Only `tools`, `prompts` and `resources` `list_changed`
  notifications reach every agent of the key; other server notifications may
  be session-scoped and are dropped (and counted).
- Request ids and progress tokens are rewritten to hub-unique numbers and
  mapped back per client, keeping their type; a client reusing an id still in
  flight gets an error for the second request, and a JSON-RPC batch gets an
  error reply. `notifications/cancelled` is translated. A client that
  disconnects has its requests cancelled upstream and their late replies
  dropped. Each client has a bounded queue (256 messages, 64 MiB) and at
  most 64 requests in flight; each upstream accepts at most 1024 in flight.
  A request over either cap gets a retryable error; a client whose queue
  overflows is disconnected; the upstream is never blocked.
- When an upstream crashes, its in-flight requests fail with a retryable
  JSON-RPC error (`-32000`, `data.retryable = true`); nothing is replayed, and
  a client that cannot even receive that error is disconnected. It restarts
  with exponential backoff from 1 to 60 seconds and is initialized again;
  requests arriving meanwhile are held and sent once it is ready, after
  `notifications/tools/list_changed`. A restart's initialize has the same 30
  second deadline; a timeout or an error reply fails the held requests
  retryably and counts as a crash. It re-initializes with the protocol
  version the first client asked for, or the one the upstream negotiated.
  Five crashes within five minutes, an initialize that does not answer
  within 30 seconds, or an upstream that cannot be registered as a host
  resource lease mark the key unhealthy for ten minutes; its clients are
  disconnected and new ones start private servers. Unhealthy marks are
  persisted in `<host state>/mcp/hub/unhealthy.json`, honoured by the next
  hub (of any build) and checked by `connect` before it attaches.
- Each upstream runs in its own process group, registered as a host
  resource lease held by the hub, and its pid, group, session and start time
  are recorded under the hub directory with the owning hub's pid, start time
  and build. On SIGTERM, SIGINT or SIGHUP the hub stops every upstream group
  before exiting (and never signals a group whose leader it already reaped).
  A hub that starts reaps a record only when its owner hub is proven dead
  (pid gone, or the pid now has another start time) and the group's leader
  still has the recorded pid and start time; a record whose owner still runs
  (an older build draining) or cannot be judged is left alone, and a group
  whose leader is gone is left too (its members cannot be proven) and
  counted. On Linux upstreams get a parent-death signal and refuse to start
  if the hub is already gone.
- An upstream stops ten minutes after its last client leaves; the hub exits
  a minute after it has no upstream (or a minute after starting, if nothing
  attaches). Nothing is pre-warmed, and the hub sleeps until its next
  deadline instead of polling.
- Upstreams receive the entry's environment (the env file included). Two
  entries with different env files, flavours or sources have different keys
  and never share.

`aethyme mcp status --json` adds the hub: whether it runs, its pid, and per
key the upstream (pid, clients, requests in flight) and counters (clients
connected and disconnected, requests, errors, cancellations, crashes,
restarts, queue overflows, refused server requests, dropped log messages,
fallbacks by reason, and the upstream's resident memory, sampled at most once
a minute). The hub records upstream starts,
stops, crashes, unhealthy keys and overflows in `events.jsonl`, never message
bodies.

## Managed servers

For some servers it is worth more than an opt-in: every agent should start
them through `aethyme mcp connect`, whatever its configuration says. A
browser-devtools server is the usual example: started through `npx` by
each agent it costs a launcher chain per session, and started through
`connect` it gets the resolved, keyed start. Such entries are *managed*.

An agent does not choose how a server starts: its host reads a
configuration and spawns the command it finds there. So enforcement is two
mechanisms, both reading the same managed list and the same matcher:

- **A. Config repair** rewrites agent configuration entries that start a
  managed server so they run `aethyme mcp connect`.
- **B. Launcher interception** puts shims named after the package-manager
  launchers first in `PATH`, so a matching launcher call lands in
  `connect` even when the configuration was never repaired.

### The managed list

Only the host file, `<host state>/mcp/services.toml`, can mark an entry
managed; a repository's `.aethyme/mcp.toml` that tries is refused
(`managed_in_repository`).

```toml
[[servers]]
name = "example-devtools"
argv = ["npx", "-y", "example-devtools-mcp"]
managed = true
match_packages = ["example-devtools-mcp"]      # a version suffix is ignored
match_executables = ["example-devtools-mcp"]   # program basenames
repair = "auto"                                # or "report"
args = "caller"                                # or "pinned"
```

- `match_packages`: npm package names. `example-devtools-mcp`,
  `example-devtools-mcp@1.2.3` and `example-devtools-mcp@latest` all match
  the package; a version written here is ignored for matching.
- `match_executables`: basenames of programs started directly, also when a
  configuration names them by absolute path.
- `repair`: whether `repair --apply --auto` (the session-start repair)
  rewrites matching entries (`auto`, the default) or only reports them.
- `args`: which arguments the server gets.
  - `caller` (the default): the entry's `argv` supplies the program
    resolution and any base arguments, and the arguments the caller gave
    after the package or program (in the agent's configuration, or of an
    intercepted launcher call) follow them. They are part of the resolved
    argv and therefore of the sharing key: two agents asking for different
    flags get different servers. Keep base arguments in the entry to what
    every caller needs; a caller's own copy of a base argument would appear
    twice.
  - `pinned`: the entry's `argv` only. A caller whose arguments differ from
    the entry's base arguments is not routed through `connect`:
    interception passes it through with a one-line note on stderr, and
    repair reports it as `arguments_differ` without rewriting it.
- Versions: a caller pinning a version (`example-devtools-mcp@1.2.3`) the
  entry does not pin gets that version, keyed separately; when the entry
  pins a version, a caller asking for another one is left alone
  (`version_differs`).
- Calls with `--help`, `-h` or `--version` after the package are never
  routed to a server (`interactive_arguments`), and neither is anything
  intercepted while standard input is a terminal: that is a person, not an
  agent host.

The matcher recognises `npx [-y] X`, `npm exec X` (and `npm x`),
`pnpm exec X`, `pnpm dlx X`, `pnpx X`, `bunx X`, `bun x X` and `yarn dlx X`
(with `--package`/`-p`: then the named binary must be listed in
`match_executables`, or equal the package's own unscoped name with that
package the only `-p`; a scoped package's last segment is never taken for
its binary), the same behind `env [NAME=VALUE ...]` or a
`node <launcher script>` interpreter, `sh -c "<one simple command>"` when
the string has no shell syntax at all (no operators, redirections,
expansions, globs or assignments), and a program whose basename is in
`match_executables`. An invocation of `aethyme mcp connect ...` never
matches. Unknown launcher options make a call unmatched rather than
guessed.

```text
aethyme mcp manage list [--json]
aethyme mcp manage add <name> --package <pkg> [--package ...] [--executable <name> ...] [--repair auto|report] [--args caller|pinned] [--json]
aethyme mcp manage remove <name> [--json]
```

`manage add` edits the host file in place (comments, order and formatting
kept) and validates the result before writing; an unknown name becomes a new
entry with `argv = ["npx", "-y", "<first package>"]` (or the first
executable), which you can edit. `manage remove` drops the managed fields
and keeps the entry. Both refresh the shims and match file when
interception is installed.

### B. Launcher interception

```text
aethyme mcp intercept install [--dir <path>] [--json]
aethyme mcp intercept status [--json]
aethyme mcp intercept uninstall [--json]
```

`install` creates the shim directory (default `<host state>/mcp/shims`,
mode 0755; `--dir` must be absolute and empty or a directory it created)
with symlinks named `npx`, `npm`, `pnpm`, `pnpx`, `bunx`, `yarn` and each
`match_executables` name, all pointing at the `aethyme` binary, and writes
the precompiled match file `<host state>/mcp/intercept.match`. It prints the
line to add where agent hosts get their environment:

```text
export PATH="<host state>/mcp/shims:$PATH"
```

It never edits a shell profile or any file outside Aethyme's state.

The binary is multi-call: invoked under a shim name it decides from
`argv[0]` before anything else (no database, configuration discovery or
Git), reads the match file and tests the call:

- no match (or a call it declines, see `args` above): it `exec`s the real
  tool, the first absolute `PATH` entry for that name that is not the shim
  directory (in any spelling, symlinks included) and does not resolve to
  this binary or to any file named `aethyme*` (a stale shim directory of
  another build). Arguments, stdio and exit status are the real tool's and
  nothing is printed; the environment gains only `AETHYME_MCP_SHIM_HOPS`.
  A chain that would pass a fifth time stops with exit 127 and a message
  naming `aethyme mcp intercept status`. With no real tool it exits 127 with
  `<name>: command not found`. Empty and relative `PATH` entries are never
  searched (a shell would search the current directory for them).
- match: it `exec`s
  `aethyme mcp connect <name> --caller-args <n> [--caller-package <spec>] --fallback -- <real launcher's absolute path> <original arguments...>`
  (the last `<n>` original arguments are the caller's) with
  `AETHYME_MCP_SHIM_GUARD=1`, `AETHYME_MCP_SHIM_DEPTH` incremented and the
  shim directory removed from `PATH`. A shim that sees the guard (or a
  depth of 2) always passes through, and the fallback names the real tool
  by absolute path, so nothing re-enters a shim. The guard is inherited by
  everything the fallback starts, and a server `connect` starts gets a
  `PATH` without the shim directory, so interception is off under a
  managed server's process tree.

Signals: Rust starts every program with SIGPIPE ignored and restores the
default disposition in the program it `exec`s, so the real tool starts with
the default SIGPIPE disposition even when its parent had deliberately
ignored it. Everything else about signals is the real tool's.

The match file is found through the host state directory
(`AETHYME_HOST_STATE_DIR`, `XDG_STATE_HOME`, `HOME`). An agent host whose
environment names a different host state directory has no match file there,
so its shims pass every call through.

An unreadable or corrupt match file never blocks a launcher: calls pass
through and the error is counted. Only hand-offs and errors are recorded
(`events.jsonl`, `command = "intercept"`); the pass-through path does no
I/O beyond reading the match file. Arguments that are not UTF-8 always
pass through.

`status` reports whether the directory and every shim exist, whether the
match file is current for the managed list (`stale` after a hand edit of the
host file: run `install` again), whether the directory is on this shell's
`PATH` ahead of every real launcher, and the recorded hand-off and error
counts. `uninstall` removes the shims, the directory when it holds nothing
else, and the match file.

### A. Config repair

```text
aethyme mcp repair [--check|--apply] [--auto] [--agent codex|claude] [--codex-config <path>] [--claude-config <path>] [--json]
```

It scans the Codex configuration (`$CODEX_HOME/config.toml`, else
`~/.codex/config.toml`), the Claude user configuration (`~/.claude.json`:
its `mcpServers` and every project's `mcpServers`) and, run inside a
repository, that repository's `.mcp.json`, for stdio entries whose command
matches a managed entry and is not already `aethyme mcp connect`, and for
`connect` entries whose absolute `aethyme` no longer exists
(`stale_aethyme_path`). `--check` (the default) lists them; `--apply`
rewrites only those entries to

```text
<absolute aethyme> mcp connect <name> [--caller-args <n> [--caller-package <spec>]] --fallback -- <original command and args>
```

keeping the entry's `env` map and every other key untouched. The command is
the stable install path of `aethyme` (the one on `PATH` when it is this
binary), so a configuration does not depend on the agent host's `PATH`. An
entry whose caller arguments are not the tail of its command (a `sh -c`
string) is reported (`arguments_not_suffix`). `--auto` writes only the
Codex TOML file, and there only entries whose managed entry says
`repair = "auto"`; the JSON files are reported (`explicit_apply_only`).
Changes take effect when the agent next starts.

Run `--apply` for `~/.claude.json` while no agent host is running: the host
rewrites that file from its in-memory copy, and a host that saves after the
repair puts the old entry back (the next `repair` finds it again).

These files hold credentials, so:

- nothing from them is printed, logged or put in `--json` except server
  names and the command's program basename (a project of the Claude file
  is shown as `project #<n>`, not by its path); a parse error names a line
  or byte offset only;
- a file that does not parse is reported and never written;
- formatting and key order are kept (TOML through a format-preserving
  editor; JSON by replacing only the edited values, in the file's own
  indentation);
- the original is first copied to a 0600 backup under
  `<host state>/mcp/repair-backups/` (never beside the configuration), the
  newest five per file kept;
- writes are compare-and-swap: size, modification time and SHA-256 are
  recorded at read, the new content is written to a temporary file in the
  same directory with the original's mode (created 0600, removed on every
  path that does not rename it, and any older than a minute is swept at
  the next run), and the original is checked unchanged right before the
  rename; a change discards the attempt, the next one waits 20 to 80 ms
  (jittered), and after three attempts the file is reported `contended`
  with nothing written. A write landing between the last check and the
  rename is not seen: that window is a few microseconds, and is the reason
  the frequently rewritten JSON file is never repaired automatically;
- a symlinked configuration is followed only to a target inside `HOME`
  (otherwise `refused_symlink`) and outside any Git checkout (otherwise
  `refused_symlink_into_repository`: the repair would be an uncommitted
  change in a dotfiles repository);
- a repository's tracked `.mcp.json` is never written: its matches are
  reported (`tracked_repository_file`), because changing it is a commit you
  make.

**Session-start repair.** With `repair_on_session_start` on, the session
start hook Aethyme already installs (`aethyme hook SessionStart`) runs
`repair --apply --auto` in-process on the Codex TOML file only, within a 2
second budget: the managed list is read first, no repository or Git is
consulted, and a file is not started with less than 600 ms left. It is silent
when nothing changed and adds one line to the session context when it
repaired something. It is off by default; turn it on in
`<host state>/mcp/settings.toml`:

```toml
[mcp]
repair_on_session_start = true
```

### Bypass detection

```text
aethyme mcp bypass [--json]
```

takes one process-table snapshot (1 second budget; Linux reads `/proc`,
macOS one `ps` listing plus the candidates' real argv and environment from
the kernel, elsewhere `ps`) and counts, per managed entry, the running
processes whose argv matches it, as `via_connect` (`connect` marks the
server it starts with `AETHYME_MCP_CONNECT`, inherited by its children; a
descendant of `aethyme mcp connect` or of the shared hub; or the command
line `connect` last exec'd), `fallback` (marked `AETHYME_MCP_FALLBACK`: the
caller's own command, run because `connect` could not start the server) and
`bypass` (everything else). Another user's processes have no readable
environment and count by ancestry only. Only counts and the matched package
or executable name are shown. When the table cannot be read
in time the result is `inconclusive`, never "none". `aethyme mcp status
--json` includes the managed list, interception state, a bypass scan and the
session-start switch under `managed`.

### What each guarantees, and the limits

- B covers every host started with the shim directory first in `PATH`,
  including plugin-bundled servers whose configuration cannot be rewritten.
  A host started from an environment without that line is not intercepted.
- B does not see a configuration that calls a server binary by absolute
  path, or a launcher by absolute path (`/usr/local/bin/npx ...`): A
  repairs those entries.
- B does not see processes started under a managed server or a fallback
  (the guard and the stripped `PATH` turn it off there).
- A covers the configuration files it knows; it cannot rewrite servers a
  plugin bundles, and a tracked `.mcp.json` needs your commit.
- Neither changes how a server already running was started; restart the
  agent.

### Enabling, step by step

1. `aethyme mcp manage add example-devtools --package example-devtools-mcp`
   (then adjust the entry's `argv` in the host file if needed).
2. `aethyme mcp resolve example-devtools` to see how `connect` will start
   it.
3. `aethyme mcp intercept install`, and add the printed `export PATH=...`
   line where your agent hosts get their environment.
4. `aethyme mcp repair` to see the configuration entries, then
   `aethyme mcp repair --apply`.
5. Optionally turn on `repair_on_session_start`.
6. Restart the agents; `aethyme mcp bypass` and `aethyme mcp status` show
   what still starts outside `connect`.

### Turning everything off

1. `aethyme mcp intercept uninstall`, and remove the `PATH` line.
2. Set `repair_on_session_start = false` (or delete `settings.toml`).
3. `aethyme mcp manage remove <name>` for each managed entry.
4. Repaired configuration entries keep working (their fallback is the
   original command); to restore one exactly, copy it back from
   `<host state>/mcp/repair-backups/`.

### Worked example: a browser-devtools server

Agents are configured with
`npx -y example-devtools-mcp@latest --headless`:

```text
$ aethyme mcp manage add example-devtools --package example-devtools-mcp
$ aethyme mcp intercept install
$ aethyme mcp repair --apply
```

An agent whose configuration was repaired now runs
`<absolute aethyme> mcp connect example-devtools --caller-args 1 --fallback -- npx -y example-devtools-mcp@latest --headless`:
`connect` starts the entry's `npx -y example-devtools-mcp` resolution with
`--headless` appended.
One whose configuration was not (a plugin-bundled server, say) still
spawns `npx -y example-devtools-mcp@latest --headless`; started with the
shim directory first in `PATH`, that `npx` is the shim, which hands the
call to the same `connect` with the real `npx` as fallback. Either way
`connect` resolves the installed copy offline and execs it, or runs the
fallback when there is none.
