# Demo script — two agents, one conflict, one recovery

Last Updated: 2026-07-17

A copy-pasteable terminal script that reproduces the broker's v0 scenario:
two agent sessions in separate worktrees, both editing the same line; the
first lands cleanly through a gate, the second is rejected at merge
simulation, recovers by following the broker's written instructions, and
lands. Written for a future recording — every command block below was
executed verbatim, top to bottom, and the output shown is captured, not
invented. Commit SHAs and gate timings will differ on your machine; the
shape of the output will not.

Prerequisites:

- `aethyme` on PATH (`cargo install --path packages/aethyme/rust/crates/aethyme-cli`)
- git ≥ 2.38 (merge simulation uses `git merge-tree`)
- `python3` (used by the demo's one gate)

The demo lives entirely in `/tmp/aethyme-demo`; delete the directory to
reset.

## Scene 1 — a toy repo, certified and gated

```bash
DEMO=/tmp/aethyme-demo
rm -rf "$DEMO"
mkdir -p "$DEMO" && cd "$DEMO"
git init -q -b main
git config user.name "Demo" && git config user.email "demo@example.com"
mkdir -p src
cat > src/greeting.py <<'EOF'
GREETING = "Hello"


def greet(name):
    return f"{GREETING}, {name}!"
EOF
git add -A && git commit -qm "chore: initial commit"

aethyme init
```

Expected (abridged — certify warns about the missing pieces, scaffold
creates exactly them):

```text
Phase 1/3 — certify (read-only):
pass     certify.git-version          git 2.55.0 (≥ 2.38 required for merge simulation)
pass     certify.git-repo             inside a git repository
...
Phase 2/3 — scaffold (deterministic, only-if-missing):
created  scaffold.config-toml         .aethyme/config.toml written — review the draft
created  scaffold.gitignore           appended the aethyme-broker block to .gitignore
created  scaffold.broker-db           integrity: ok

Phase 3/3 — gates draft (adaptive):
warn     gates.draft                  no manifests recognized — define .aethyme/gates.toml yourself; until then the broker runs conflict-only (no verification)
```

Commit the scaffold and give the repo one regulation — a cheap compile
gate (the toy repo has no recognized manifest, so we write it by hand):

```bash
git add .gitignore .aethyme/config.toml
git commit -qm "chore: adopt aethyme broker (scaffold)"

cat > .aethyme/gates.toml <<'EOF'
[[gate]]
name = "py-compile"
command = "python3 -m compileall -q src"
cost = 1
triggers = ["src/**/*.py"]
EOF
aethyme broker gates validate
git add .aethyme/gates.toml && git commit -qm "chore: add py-compile gate"
```

```text
gates.toml OK — 1 gate(s), cheap-first:
  [1] py-compile — python3 -m compileall -q src (triggers: src/**/*.py)
```

## Scene 2 — two agents, two worktrees

This scene uses hand-made worktrees to show the attach path: the worktrees
are ordinary `git worktree` checkouts, and `adopt` registers them as
sessions. In normal agent use, `aethyme broker start --task "..."` creates
and registers the isolated worktree in one step; `adopt` remains the path
when a vendor tool already made the checkout.

```bash
git worktree add -q -b agent/alpha .aethyme/worktrees/alpha main
git worktree add -q -b agent/beta  .aethyme/worktrees/beta  main

cd "$DEMO/.aethyme/worktrees/alpha"
aethyme broker adopt --task "Make the greeting French"
cd "$DEMO/.aethyme/worktrees/beta"
aethyme broker adopt --task "Make the greeting shout"
aethyme broker agents
```

```text
Adopted session 1 — worktree /private/tmp/aethyme-demo/.aethyme/worktrees/alpha on branch agent/alpha
Adopted session 2 — worktree /private/tmp/aethyme-demo/.aethyme/worktrees/beta on branch agent/beta
ID   STATUS   ORIGIN   BRANCH                   TASK
1    active   adopted  agent/alpha              Make the greeting French
2    active   adopted  agent/beta               Make the greeting shout
```

## Scene 3 — alpha lands first

Both agents are about to change the same `GREETING` line. Alpha commits
and submits first: merge simulation is clean, the gate runs on the merged
tree, and the entry auto-promotes onto the local `aethyme/integration`
branch.

```bash
cd "$DEMO/.aethyme/worktrees/alpha"
cat > src/greeting.py <<'EOF'
GREETING = "Bonjour"


def greet(name):
    return f"{GREETING}, {name}!"
EOF
git add src/greeting.py && git commit -qm "feat: greet in French"
aethyme broker submit --session 1
```

```text
Submitting session 1 — HEAD 9604cc6b9e4b
  9604cc6 feat: greet in French
gate py-compile started (cost 1)
gate py-compile           pass in 161ms
gate wall time: 161ms
entry 1 → promoted (auto-promoted)
What now: aethyme/integration is at a51f7e2a42df and contains this work. Your checkout and branches are untouched — keep working, or start a follow-up with `aethyme broker adopt --reuse --task "..."`, or finish with `aethyme broker close --session 1`.
```

## Scene 4 — beta is rejected in milliseconds

Beta edits the same line and submits. The simulation detects the textual
conflict **before any gate runs** — no CI minutes are spent on a doomed
merge — and the broker writes recovery instructions into beta's worktree.

```bash
cd "$DEMO/.aethyme/worktrees/beta"
cat > src/greeting.py <<'EOF'
GREETING = "HELLO"


def greet(name):
    return f"{GREETING}, {name}!"
EOF
git add src/greeting.py && git commit -qm "feat: shout the greeting"
aethyme broker submit --session 2
```

```text
Submitting session 2 — HEAD f144b72ad4a6
  f144b72 feat: shout the greeting
✗ conflict — rejected before any gate ran. Conflicting files:
  - src/greeting.py
Instructions written to the session worktree at .aethyme/broker-action-required.md
Quick start: git fetch . a51f7e2a42df790a5804319cc01bbc0c5ffd1882 && git rebase a51f7e2a42df790a5804319cc01bbc0c5ffd1882   (then resubmit)
Error: submission conflicted
```

This is the vendor-neutral hand-back: any agent that can read a file in
its own worktree gets the conflicting files, the blocking session, and the
exact recovery commands.

```bash
cat .aethyme/broker-action-required.md
```

```text
# Broker: action required — merge conflict

Your submission (commit `f144b72ad4a622cb757ec9f805a139de11cff890`) conflicts with the integration
branch (base `a51f7e2a42df790a5804319cc01bbc0c5ffd1882`) and was rejected before any CI ran.

Conflicting files:
- src/greeting.py

No live session currently holds leases on these paths.

To resolve, in this worktree:

1. `git fetch . a51f7e2a42df790a5804319cc01bbc0c5ffd1882` (the base is a local commit; no network)
2. `git rebase a51f7e2a42df790a5804319cc01bbc0c5ffd1882` and resolve the conflicts above
(headless agents: if the rebase pauses, continue with
`GIT_EDITOR=true git rebase --continue` — never rely on an
interactive editor)
3. resubmit: `aethyme broker submit --session 2`

This file is regenerated on each conflicted submission; it is
gitignored broker state (delete freely).
```

(When a live session holds leases on the conflicting paths at rejection
time, this file names it instead: "Blocking session(s): 1 — coordinate or
wait for their promotion.")

## Scene 5 — recovery

The file quotes the exact base SHA; `aethyme/integration` is the same
commit, so the branch name works too. The rebase stops on the conflicting
file; resolve it (here: keep both intents — French *and* shouted),
continue headlessly, resubmit.

```bash
git rebase aethyme/integration || true
git status --short          # UU src/greeting.py

cat > src/greeting.py <<'EOF'
GREETING = "BONJOUR"


def greet(name):
    return f"{GREETING}, {name}!"
EOF
git add src/greeting.py
GIT_EDITOR=true git rebase --continue
aethyme broker submit --session 2
```

```text
Successfully rebased and updated refs/heads/agent/beta.
Submitting session 2 — HEAD 62f85cd2d69f
  62f85cd feat: shout the greeting
gate py-compile started (cost 1)
gate py-compile           pass in 93ms
gate wall time: 93ms
entry 3 → promoted (auto-promoted)
What now: aethyme/integration is at a6330f1a40c5 and contains this work. Your checkout and branches are untouched — keep working, or start a follow-up with `aethyme broker adopt --reuse --task "..."`, or finish with `aethyme broker close --session 2`.
```

## Scene 6 — the whole picture

Status shows both sessions, the superseded first attempt, and both
promotions; the integration branch holds the serialized result; the event
log ([`events-contract.md`](events-contract.md)) has the full flight
recording.

```bash
aethyme broker status
git log --oneline aethyme/integration
aethyme broker events --kind merge.
aethyme broker close --session 1
aethyme broker close --session 2
```

```text
Integration: aethyme/integration @ a6330f1a40c5

ID   STATUS   ORIGIN   BRANCH                   TASK
1    active   adopted  agent/alpha              Make the greeting French
2    active   adopted  agent/beta               Make the greeting shout

QID  SID  QSTATUS     HEAD
1    1    promoted    9604cc6b9e4b
2    2    superseded  f144b72ad4a6
3    2    promoted    62f85cd2d69f

62f85cd feat: shout the greeting
9604cc6 feat: greet in French
ecf93fa chore: add py-compile gate
70a7fd1 chore: adopt aethyme broker (scaffold)
baf76b3 chore: initial commit
```

Abridged event log — note `merge.conflict` carries the conflicting paths,
and each `merge.verified` records which gates ran on which merged tree:

```text
merge.submitted              sid=1 {"head":"9604cc6b..."}
merge.verified               sid=1 {"gates":[{"cached":false,"gate":"py-compile","status":"pass"}],...}
merge.promoted               sid=1 {"branch":"aethyme/integration",...}
merge.submitted              sid=2 {"head":"f144b72a..."}
merge.conflict               sid=2 {"conflicts":["src/greeting.py"],...}
merge.submitted              sid=2 {"head":"62f85cd2..."}
merge.superseded             sid=2
merge.verified               sid=2 {"gates":[{"cached":false,"gate":"py-compile","status":"pass"}],...}
merge.promoted               sid=2 {"branch":"aethyme/integration",...}
```

## Recording notes

- Total runtime is a few seconds; for a recording, pause on the
  action-required file (Scene 4) — it is the product's voice.
- The promotion is local by design: `aethyme/integration` never pushes.
  End the recording on `git log --oneline aethyme/integration` to make
  that landing strip visible.
- Reset between takes with `rm -rf /tmp/aethyme-demo`.
