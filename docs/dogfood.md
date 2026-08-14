# Broker v0 Dogfood Playbook (issue #33)

Goal: use `aethyme broker` for real multi-agent development on this
repository for about a week, with at least two agent vendors attached
concurrently, and produce a friction log with cost/time accounting.

**Kill criterion (decided 2026-07-10):** the project stops if costs
increase too much AND no time is saved. The accounting below is how we
find out.

## One-time setup

```bash
# Build the release binaries and put them on PATH
cd packages/aethyme/rust && cargo build --release --bin aethyme && cd -
export PATH="$PWD/packages/aethyme/rust/target/release:$PATH"

# NOTE (friction item #31): pip's Python entrypoint is also called
# `aethyme` — whichever is first on PATH wins. The line above puts the
# Rust binary first for this shell. Log every time this bites.

aethyme broker gates validate   # sanity-check .aethyme/gates.toml
```

Gates and broker config are committed (`.aethyme/gates.toml`,
`.aethyme/config.toml`); everything the broker writes at runtime
(`broker.db`, `logs/`, `run/`, `worktrees/`) is gitignored.

## The daily loop

```bash
# Starting an agent on a task — normal path:
aethyme broker start --task "short task description"
cd <reported-worktree>

# If you or your agent tool already created a dedicated worktree:
cd <worktree> && aethyme broker adopt --task "short task description"

# Before planned shared edits:
aethyme broker leases claim <path> --session <id>

# For broad rewrite tools:
aethyme broker exec --session <id> -- <command>

# Or let the broker create worktree + branch + spawn in one step:
aethyme broker start-agent --task "port X" --cmd "claude -p '...'"

# The picture, any time (also refreshes leases → overlap warnings):
aethyme broker status
aethyme broker events --since <id>       # or --follow in a spare terminal

# An agent's work is committed and ready:
aethyme broker submit --session <id>     # simulate → affected gates on merged tree
                                         # → auto-promotes to the LOCAL integration
                                         # branch when verified (default; [promote]
                                         # mode = "manual" for an explicit step)

# Shipping remains explicit and authorized, but coordinated through the broker:
aethyme broker git --session <id> --repo <owner/name> --reason "authorized release" -- push origin <refspec>
aethyme broker gh --session <id> --repo <owner/name> --reason "authorized release" -- pr create ...

# Done with a session:
aethyme broker cleanup <id>              # refuses if work would be lost; --force discards
```

Multi-vendor requirement: at least once, run Claude Code and a second
vendor (Codex, Aider, anything) as concurrent adopted/spawned sessions.
The broker is vendor-blind; this validates the neutral-substrate thesis.

## What to log

Append to `docs/dogfood-friction.md` as it happens, not at the end:

- **Blockers** — anything that made you fall back to raw git.
- **Noise** — warnings/rejections that were wrong or unhelpful.
- **Catches** — collisions or conflicts the broker caught that would have
  cost you time.
- **Gaps** — things you reached for that don't exist.

Each entry: date, what happened, cost (minutes lost or saved), and
whether it should become an issue.

## The accounting

The event log has the receipts:

```bash
aethyme broker events --json | jq -r .kind | sort | uniq -c
```

- CI avoided: `gates run`/`submit` outcomes with `"cached": true`
  (dedup) plus gates *not selected* for docs-only diffs.
- Conflicts caught pre-CI: `merge.conflict` events — each one cost zero
  gate runs.
- Cost side: setup time, minutes lost to friction-log blockers, any
  token spend caused by broker workflow (e.g. agents re-reading
  action-required files).

End-of-week verdict: weigh the two columns against the kill criterion.

## Known caveats going in

1. `aethyme` PATH collision with the pip entrypoint (#31).
2. Gates in simulated worktrees reuse the main checkout's `.venv` (see
   comments in gates.toml) — a diff that changes Python dependencies can
   pass gates it shouldn't. Rare; log it if it happens.
3. First `cargo-test` gate per machine cold-builds into the shared
   `~/.cache/aethyme-cargo-target` (slow once, warm after).
4. Idle/stale thresholds are constants (10 min / 2 h). If they misjudge
   your agents, that's friction-log material — making them config is a
   one-liner.
