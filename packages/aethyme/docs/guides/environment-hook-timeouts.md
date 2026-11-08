# Environment hook timeouts

Last Updated: 2026-11-08

Each hook (`provision`, `build`, `template`, `stop`, `destroy`, ...) runs in
its own process group under a deadline. The deadline defaults to 600 s. A
component whose hooks need longer, such as a seed that migrates a fresh
cluster, sets its own:

```toml
[[profiles.components]]
name = "seed"
runtime = "hooks"
depends_on = ["stack"]
provision = "bash scripts/dev/backend-refresh.sh"
destroy = "true"
hook_timeout_seconds = 2400
```

The value applies to every hook of that component and must be positive.
It covers the `build` a shared environment runs in a fresh export when it
redeploys, which is where a dependency install (a virtualenv,
`node_modules`) most often outgrows 600 s; a timed-out hook's error names the
setting. Declaring `cache_inputs` and `build_output` on such a component is
the better fix: a redeploy whose build inputs did not change restores the
cached output instead of rebuilding it. Prebuilds keep their own
`timeout_seconds`.

A failure to run or clean up a shared redeploy's build, rather than the build
failing, is retried once (a `redeploy build retried` note): for example a
process left in the hook's group that was still exiting after SIGKILL. After
SIGKILL the group gets up to 3 s to disappear before that counts as a failure.

Each hook phase logs to its own file beside the runtime log,
`<component>.<phase>.log` (`build`, `provision`, `template`, `stop`,
`destroy`), and every run starts with a timestamped
`[aethyme <time>] <component>: <phase>: started` marker. `aethyme env logs
<id> <component> --phase build --run last` shows only the latest run, and a
failure recorded in `last_error` or `env history` carries `log` {`path`,
`offset`}: the file and byte offset where that run begins.

When a hook is stopped at its deadline, or preempted, everything it had
printed up to that point is appended to its phase log, followed by the
reason it was stopped. If stopping it fails too, for example because a
process in the hook's group outlived SIGKILL, the partial output and that
error are still logged.

Stopping a hook does not stop containers it started: the container engine
runs them. `compose down` removes the project's own containers, including
`compose run` one-offs, but not every container that carries the project's
label. When a `compose` component is destroyed, any container still labelled
with its exact compose project after `down` is force-removed, and `down` runs
again so the project's network and volumes go too. The component log records
how many were removed.

Log retention is a host setting, in `<host state>/environments.toml`:

```toml
[logs]
max_bytes = 8388608  # rotate a log past this size (default 8 MiB, at least 64 KiB)
keep = 3             # rotated files kept per log (default 1; 0 discards a full log)
```

A running supervisor reads it when it starts.

