# Broker benchmark, repetition 1 (P5.6)

Last Updated: 2026-09-26

In this run the Aethyme broker was the only workflow where no broken merge reached main. It caught both deliberate conflicts before they merged. It finished in the same wall time as the other two workflows, and used about 27% more tokens. That is one repetition per workflow, so it is a demonstration, not a measurement: the result shows what the gate does, not how often it wins.

## Setup

- **Harness:** `packages/aethyme-eval/benchmarks/broker/`. The protocol is in its README.
- **Fixture:** `shoplib`, a synthetic order-management library (standard-library Python: 22 modules, 24 tests).
- **Tasks:** 12, three per agent slot, each with a hidden check. Three overlaps are built in:

  | Overlap | Tasks | What happens |
  | --- | --- | --- |
  | Textual | t01 / t04 | Both change `format_price` |
  | Semantic | t05 / t08 | A rename plus a new caller: the merge is clean but the tests fail |
  | Shared file | t03 / t09 | Both edit `registry.py` |

- **Agents:** four at once, each launched in its own Chau7 tab. Each slot kept the same model in every arm.

  | Slot | Agent |
  | --- | --- |
  | s1 | Claude Code, Sonnet 5 |
  | s2 | Claude Code, Opus 5.5 |
  | s3 | Codex, GPT-6-Luna (high reasoning) |
  | s4 | Codex, GPT-6-Sol (high reasoning) |

- **Workflows compared:**

  | Arm | Workflow |
  | --- | --- |
  | A | Plain `git worktree` + a local PR queue (CI on the branch, merge, CI on main) |
  | B | The agents' native worktrees (`claude --worktree`, `codex --worktree`) + the same queue |
  | C | Aethyme broker (`start`, `submit`, `finish`), with a merged-tree gate running the unit suite |

- **Version:** Aethyme v0.8.4.
- **Date:** 2026-09-26.

## Results

| Measure | A: git worktree + PRs | B: native worktrees | C: Aethyme broker |
| --- | --- | --- | --- |
| Hidden checks passing | 12/12 | 12/12 | 12/12 |
| Overlaps resolved | 3/3 | 3/3 | 3/3 |
| Final main green | yes | yes | yes |
| Conflicts caught before merge | 1 | 0 | **2** |
| Broken merges reaching main | 1 | 1 | **0** |
| Launch to last landing | 249 s | 224 s | 219 s |
| Agent turns (model calls) | 88 | 86 | 105 |
| Tokens (mostly cache reads) | 4.56M | 4.55M | 5.77M |
| Operator interventions, excluding launch friction | 0 | 0 | 0 |

- **Arm A:**
  - The semantic overlap merged and turned main red twice (t08, then t06) before a follow-up fixed it.
  - Branch CI caught one stale branch (t12).
  - The textual overlap was rebased by the agent, not caught.
- **Arm B:** the semantic overlap merged red once (t05, attempt 1), and the next attempt fixed it.
- **Arm C:** the broker caught both conflicts before merge.
  - The textual conflict was refused at `submit` (exit 3, t04). The agent rebased and resubmitted.
  - The semantic conflict failed the gate on the merged tree (exit 4, t08). The agent that caused it fixed it before it landed.
  - This fix is where most of arm C's extra tokens went: slot s3 used 1.24M tokens in C against about 0.5M in A and B.

## Reading the numbers

- **Fewer broken merges on main is the one claim this run supports.** It comes from the merged-tree gate, and the other two workflows had no equivalent: their CI ran on the branch, or on main after the merge.
- **Wall time is a tie.** The broker's gate added seconds, not minutes, on a 0.1 s test suite. A slower suite would change this.
- **The token cost (+27%) is the price of making the author fix the break.** In A and B, the break landed and someone else paid for the fix afterwards. That cost doesn't show up here, because the next agent happened to fix main within a minute.

## Limits

- **N=1 per arm, with nondeterministic agents.** Another repetition could reorder every row.
- **The fixture is tiny.** The tasks finished in under 4 minutes, where the harness estimated 45–75.
- **Launch friction was the only operator work.** It is excluded above:
  - Claude Code's folder-trust dialog. In arm B, `--worktree` needs the exact folder trusted.
  - Prompts lost by the TUI launcher.

  The per-arm logs still record it: A had 3 launch interventions, B 3, C 1. C was lower only because its folder was trusted before the clock started.
- **Arm order was A, B, C,** not the protocol's C, A, B, because arm C waited for the maintainer's `broker trust`.
- **Arm A's operator log overstates its wall time** (1,324 s), because the end was logged late. The table uses the last landing instead, for every arm.
- **Tokens count only each slot's main session.** A globally installed security-review hook started extra sessions in every arm; those are excluded.
- **Effort differs by slot.** Sonnet ran at high effort and Opus at medium, the Claude Code defaults. This was the same in every arm.

## Raw data

The raw data is in `packages/aethyme-eval/benchmarks/broker/results/r1/`: per-arm `score.json`, the operator logs, the merge logs (A and B), and the broker submit log (C).
