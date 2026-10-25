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

This command does not create files, install or activate a scheduler, poll a
provider, contact Chau7, or change session monitoring. Installation, lifecycle
commands and health reporting remain outstanding for #887. The current manual
monitor is documented in `scripts/adapters/PR-MONITOR.md`; its paired helpers
already bound tick/list/delivery work to one shared pass budget.
