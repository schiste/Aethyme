# Environment hook timeouts

Last Updated: 2026-11-03

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
Prebuilds keep their own `timeout_seconds`.

When a hook is stopped at its deadline, or preempted, everything it had
printed up to that point is appended to the component log, followed by the
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
