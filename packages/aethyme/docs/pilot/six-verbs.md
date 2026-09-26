# The six broker verbs

Last Updated: 2026-09-26

Daily use needs six commands. Everything else is under
`aethyme broker advanced` and you can ignore it until something asks for it.
Add `--json` to any of them for structured output; `--help` on any verb
lists its forms.

| Verb | When | What it does |
| --- | --- | --- |
| `start` | Before an agent edits anything | Creates an isolated worktree and a session for one task |
| `status` | Any time | Sessions, overlaps, the merge queue, integration, and advice with the next command |
| `submit` | When the agent has committed | Merges onto `aethyme/integration` in a simulation, runs the gates, promotes on success |
| `finish` | After a successful submit | Closes the session when nothing is left unsubmitted |
| `unblock` | When something says it is blocked | Lists blockers with the command that clears each; clears one by id |
| `gc` | Weekly, or when disk fills | Plans and applies cleanup of old worktrees and build output |

## start

```bash
aethyme broker start --task "Add rate limiting to the upload API" --agent "Ada <ada@example.com>"
```

Prints the session id and the worktree path. Run one agent per session, in
that worktree only. Useful forms:

- `--path src/upload/` claims a path up front (a trailing `/` claims a
  directory), so other sessions see the overlap before they touch it. Repeat
  it for more paths.
- `start --adopt --task "..."` registers the worktree you are already in.
- `start --reuse --task "..."`, run in a session's worktree, points that
  session at a follow-up task.

## status

```bash
aethyme broker status
aethyme broker status --summary
```

Overlaps warn; they never block. Read the `advice` lines: each names the
session it concerns and the command to run next. `--summary` skips the
per-session detail and is faster on busy repositories.

## submit

```bash
aethyme broker submit --session 12
```

Only committed work is submitted, so commit first. Gates run on the merged
tree, not on the worktree. Promotion is local: `submit` never pushes, and
publishing `aethyme/integration` stays your decision. If another session got
there first and the merge conflicts, `submit` refuses and writes
`.aethyme/broker-action-required.md` into the worktree with the files, the
other session and the rebase steps. Resolve, commit, submit again.

## finish

```bash
aethyme broker finish --session 12
```

Refuses, and prints the next command, while the worktree has uncommitted
changes or commits that were never submitted. Pass `--keep-worktree` to keep
the checkout after closing.

## unblock

```bash
aethyme broker unblock              # list every current blocker
aethyme broker unblock lease:42     # clear one
```

Each blocker has a stable id, its cause, and the exact command that clears
it. Some need more from you, such as `--reason`, or `--outcome` after you
have checked the remote yourself. A refusal changes nothing, says what it
needs, and exits 3. (v0.8.3: list with `aethyme broker blockers`.)

## gc

```bash
aethyme broker gc plan
aethyme broker gc apply --confirm <sha256>
```

`plan` lists what it would remove and prints a digest; `apply` removes
exactly that plan and nothing else.

## An example session

Two agents, one repository.

```bash
# Agent A's task
aethyme broker start --task "Add rate limiting to the upload API" --path src/upload/
#   -> session 12, worktree .../upload-rate-limit

# Agent B's task, started while A works
aethyme broker start --task "Rename the upload size setting"
#   -> session 13

aethyme broker status
#   B has edited src/upload/config.rs, which session 12 claimed:
#   status reports the overlap. Nobody is blocked; you decide who goes first.

# A commits in its worktree, then:
aethyme broker submit --session 12      # gates pass, promoted
aethyme broker finish --session 12

# B commits, then:
aethyme broker submit --session 13
#   conflicts with 12's promoted change: refused, and
#   .aethyme/broker-action-required.md tells B how to rebase.
# B rebases onto aethyme/integration, commits, and resubmits:
aethyme broker submit --session 13      # promoted
aethyme broker finish --session 13

# End of the week
aethyme broker gc plan
aethyme broker gc apply --confirm <sha256 printed by plan>
```

When you are ready to share the result, push `aethyme/integration` (or merge
it into your default branch) the way you normally would.
