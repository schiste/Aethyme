# PR-watch service planning

Last Updated: 2026-10-25

The built-in service foundation is an explicit read-only macOS launchd plan:

```sh
aethyme broker advanced watch pr service plan --broker-bin /absolute/path/to/aethyme --seconds 300 --json
```

Use the router from an immutable versioned runtime, not a temporary worktree or
a mutable global link. The command canonicalizes existing paths and records
SHA-256 identities for the router, Python interpreter and paired embedded
monitor/delivery scripts. Executable identity reads are capped at 256 MiB.
It requires the repository's existing broker UUID and a non-root Unix user;
it does not create a repository identity. Planning is also available on Unix
hosts for inspecting a proposed macOS unit. It is not a Linux service installer.

The versioned JSON schema is `aethyme.pr-watch-service-plan/v1`. `digest` is
SHA-256 over the serialized plan with its digest field empty, in struct field
order. It binds the repository, user, canonical paths, executable identities,
helper identities, interval and complete unit text. A future installer must
recompute the plan rather than trusting paths in an imported JSON document.

The proposed helper destination is under the user's Library/Application Support/
Aethyme/watch-services/<repository-uuid>, independent of disposable source
worktrees. The proposed unit is a per-user LaunchAgent. Values are XML escaped,
arguments are separate array entries, the interval is 120 through 3600 seconds,
and RunAtLoad is false. The generated unit directs stdout and stderr to
/dev/null; durable bounded health reporting is required before activation.

The plan command does not create files, install or activate a scheduler, poll a
provider, contact Chau7, or change session monitoring. Scheduler lifecycle,
generation-bound health interpretation and live qualification remain open for #887. The current manual
monitor is documented in `scripts/adapters/PR-MONITOR.md`; its paired helpers
already bound tick/list/delivery work to one shared pass budget.

## Explicit file installation (macOS)

After reviewing the plan, repeat its exact inputs and confirm its digest:

```sh
aethyme broker advanced watch pr service install --broker-bin /absolute/path/to/aethyme --seconds 300 --confirm <plan-digest> --json
```

The command recomputes the entire plan, including executable identities. It
accepts no imported plan JSON. Missing or malformed confirmation refuses before
broker discovery, and a changed plan refuses before filesystem writes. Existing
repository identity is read from a snapshot; compatibility snapshot mode cannot
install files. Installation is available only on macOS; pure filesystem controls
run against temporary fixtures on Unix.

The installer validates directory ownership and permissions and refuses symlink
components. It creates private helper directories, copies the frozen pair, writes
`plan.json`, and publishes the reviewed unit last. Each file is capped at 64 KiB.
Exact existing files are reusable; differing files are preserved and refused.
The runtime executables are referenced, not copied into each service directory.

A persistent private installer lock serializes one fixed scratch file. It is
never deleted to steal another writer's claim. Publication cannot overwrite an
existing destination. A crash between creating a destination and releasing the
scratch link is recovered only when both expected names have the same inode and
exact reviewed bytes. Unknown links are refused. Repeated interruptions cannot
accumulate new scratch names. Installation may leave a safe partial prefix;
repeat the same confirmed plan to complete it after inspecting an error.

The JSON result is `aethyme.pr-watch-service-install/v1`: `plan_digest`,
`created_files`, `activation_performed: false`, and `scheduler_state:
"not_observed"`. Files being present does not imply a service is loaded or
unloaded. Installation never invokes launchctl, a provider or the monitor.
Activation, status, deactivation and removal are not implemented yet; do not
manually enable this proposed service before generation and runtime identity
checks have been qualified. Existing manual monitoring remains separate.
