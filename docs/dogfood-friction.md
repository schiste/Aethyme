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
| 2026-07-13 | note | Decision flip: verified submissions now promote immediately (auto is the default; manual stays as config). Rationale: first smoke showed the verify pipeline trustworthy; a human promote step re-inserts the bottleneck the broker removes. | 0 | changed default |
| 2026-07-13 | note | Clean redeploy: scaffold + certify green in <1s on fresh broker state. Certification found nothing to complain about — the granular split held up in practice. | 0 | accepted |
| 2026-07-13 | catch | Real dogfood task: #40 (integration follows main) developed in an adopted worktree, submitted through the broker, verified by the cargo gate on the simulated merged tree (~44s, shared target cache), auto-promoted, merged to main. First issue closed by the machinery it fixes. | +10 | fixed via broker |
| 2026-07-13 | note | Gate selection precision confirmed: .rs+.md diff triggered ONLY cargo-test — ruff/pytest correctly skipped. Cold-ish cargo gate: 44s wall (cache pre-warmed by dev run); acceptable for Rust changes. | 0 | accepted |
