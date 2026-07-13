# Broker v0 Friction Log

Append-only. Each entry: date, type (blocker / noise / catch / gap),
what happened, cost or saving in minutes, action (issue filed / accepted / fixed).

| Date | Type | What happened | ± min | Action |
|------|------|---------------|-------|--------|
| 2026-07-13 | gap | Known going in: pip `aethyme` entrypoint shadows the Rust binary unless PATH is prefixed per shell. | -2/shell | #31 (already filed, priority:high) |
| 2026-07-13 | catch | First real submit: ruff gate on the merged tree rejected 27 pre-existing lint violations on main that CI never runs ruff for. Fixed on main (b8b9cb2). | +15 | fixed |
| 2026-07-13 | gap | Integration branch is created once from main and never follows it — after main advanced, resubmits verified against a stale base. Manual `git update-ref refs/heads/aethyme/integration main` works; needs a `broker` command or auto-refresh policy. | -5 | file issue |
| 2026-07-13 | catch | pytest gate in the merge-sim worktree exposed that evals/tools/aethyme.toml hardcoded .venv/bin/python — eval self-tool tests failed in ANY venv-less checkout (worktrees). Fixed with {{TOOL_PYTHON}} placeholder (b503c3e). | +20 | fixed |
| 2026-07-13 | note | Full submit (ruff + 445-test pytest on simulated merged tree): ~35s wall. Acceptable; cargo gate still untested cold. | 0 | accepted |
