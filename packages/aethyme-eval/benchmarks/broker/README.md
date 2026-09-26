# Broker benchmark (Phase 5, P5.6)

Does the Aethyme broker land concurrent agent work more safely or cheaply
than plain worktrees plus pull requests? Four agents do twelve small tasks
on a throwaway Python repository, with three deliberate overlaps, under
three coordination arms. This directory holds the harness. It never launches
agents itself: the coordinator launches them through Chau7.

Cardinal rule 1 applies: every setup script refuses a destination inside
any Git work tree, so the playground is always its own repository, never
Aethyme.

## Contents

| Path | Purpose |
| --- | --- |
| `fixture/` | The playground source: `shoplib`, a stdlib-only order library (22 modules, 14 test files, 24 tests, under 0.1 s). |
| `setup_playground.sh <dest>` | Copies the fixture to `<dest>`, `git init`, and makes a deterministic base commit tagged `bench-base`. |
| `tasks.yaml` | Slots and their models, 12 tasks (3 per slot), the 3 overlaps, and each task's acceptance text and hidden check. |
| `arm_{a,b,c}_setup.sh <root> [--rep N]` | A fresh arm root and `<root>/launch_plan.json`. |
| `merge_queue.py` | The local PR flow for arms A and B (copied into each arm root as `bin/pr`). |
| `score.py` | Scores one arm root. The input formats are in its docstring. |
| `oplog.sh` | Appends start, end and intervention events to an arm's operator log. |
| `hidden_checks/` | One check per task plus one per overlap. Agents never see them. |
| `reference/` | `tNN.patch` solves one task on the base; `integrated.patch` solves all twelve with the overlaps resolved. |
| `selftest.py <scratch>` | A dry run of all of the above, with no agents and no broker. |

## Workload

| Slot | Model | Position 1 | Position 2 | Position 3 |
| --- | --- | --- | --- | --- |
| s1 | Claude Code, `sonnet` | t01 currency symbols in `format_price` **[textual]** | t02 postal-code validation | t03 TSV exporter **[shared]** |
| s2 | Claude Code, `opus` | t04 thousands separator in `format_price` **[textual]** | t05 rename `Inventory.reserve` to `reserve_stock` **[semantic]** | t06 express shipping |
| s3 | Codex, `gpt-6-luna` | t07 case-insensitive search with a tag filter | t08 subscription renewals, a new caller of `reserve` **[semantic]** | t09 `low-stock` CLI command **[shared]** |
| s4 | Codex, `gpt-6-sol` | t10 loyalty tiers | t11 top-SKUs report | t12 accent-folding `slugify` |

Each overlap pair spans two slots at the same queue position, so both tasks
are in flight at the same time:

- **Textual (t01 × t04).** Both change the signature and body of
  `money.format_price`, so Git reports a merge conflict.
- **Semantic (t05 × t08).** t05 renames `Inventory.reserve`. t08 adds a new
  module that holds stock "the same way `OrderService.place` does", which
  means calling `reserve`. The files are disjoint, so Git merges cleanly,
  but the merged tree fails t08's tests. If t05 lands before t08 starts, t08
  sees the new name and the conflict disappears. That is a legitimate
  outcome, so the arm's report must record it.
- **Shared file (t03 × t09).** Both add an entry to `shoplib/registry.py`:
  t03 in `EXPORTERS` at the top and t09 in `COMMANDS` at the bottom. Git
  merges cleanly. This pair measures false refusals and over-serialisation.

The slot-to-model assignment comes from `tasks.yaml` and is the same in every
arm. Claude slots run with `--permission-mode bypassPermissions`. Codex slots
run with `-a never -s danger-full-access -c model_reasoning_effort=high`,
because a sandboxed Codex cannot write the bare remote (A, B) or the broker
state (C). The high reasoning setting overrides the `max` in
`~/.codex/config.toml`, where `gpt-6-luna` is the configured default. Every
agent is unattended and unsandboxed inside a scratch directory.

## Arms

| Arm | Isolation | Landing | Landing ref scored |
| --- | --- | --- | --- |
| A | `git worktree add <root>/wt/<slot>` per slot, created by the setup script | `bin/pr open` pushes to `pr/<task>` on a local bare `remote.git`. `bin/pr wait` blocks on the merge queue. | `remote.git` `main` |
| B | The agent's native worktree: `claude --worktree bench-<slot>`, `codex --worktree` | Same as A | `remote.git` `main` |
| C | `aethyme broker start` (worktrees under `<root>/broker-worktrees` through `AETHYME_WORKTREE_ROOT`) | `broker submit`: merged-tree gate `unit-tests` from `.aethyme/gates.toml`, then promotion. Then `broker finish`. | `repo` `aethyme/integration` |

The merge queue (A and B) behaves like GitHub with required CI but without
"require branches to be up to date". CI runs on the pushed branch alone. A
textual conflict is rejected (exit 3), a red branch is rejected (exit 4), and
otherwise the PR merges with `--no-ff`, main is pushed and main's suite runs
again (exit 5 if it is now red). Agents are told each exit code and what to
do about it. A and B get the same feedback loop. The only difference between
them is who creates the worktree.

Arm C prompts use the v0.8.4 public verbs (`start`, `submit`, `finish`,
`status`). **Install v0.8.4 before running arm C.** 0.8.3 still has the old
spellings. In arm C, `bin/aethyme` is a transparent wrapper: it runs the real
binary and appends the argv and exit code of every call to `broker_log.jsonl`.

## Protocol (coordinator)

Run one arm at a time. Order: C, A, B for repetition 1, then A, B, C for
repetition 2, so that neither arm always goes first. Use a fresh root outside
every repository, for example `~/bench-p56/arm-a-r1`.

1. **Preflight (once).** `aethyme --version` must report 0.8.4.
   `claude --version` and `codex --version` must work. Record the state of the
   globally enabled `aethyme@aethyme` Claude plugin, and keep it the same in
   every arm (see the threats below). Run
   `python3 selftest.py /tmp/bench-selftest`; it must end with
   `0 failure(s)`.
2. **Set up.** `./arm_<x>_setup.sh <root> --rep <n>`. Read
   `<root>/launch_plan.json`.
   - Arm C only: from a real terminal (a Chau7 tab is fine), `cd <root>/repo`
     and run `aethyme init` then `aethyme broker trust`. `trust` needs an
     interactive terminal. Commit anything `init` writes, before the launch.
     Check with `aethyme broker status --json`.
   - Arms A and B only: start the queue in its own tab:
     `python3 <root>/bin/merge_queue.py --arm-root <root> serve`.
3. **Launch.** Run `./oplog.sh <root> arm_start`. Then, for each of the four
   slots, call Chau7 `agent_launch` with `directory = slots[i].directory`,
   `agent_command = slots[i].agent_command` and `prompt = slots[i].prompt`
   (also saved in `<root>/prompts/<slot>.md`). Launch all four within one
   minute. Tag each run `launch_plan.chau7_tag` plus the slot (`run_tag`).
4. **Monitor.** Watch with `tab_status` and `tab_output`. Intervene only
   when an agent is stuck for more than 10 minutes, asks a question, or has
   stopped before landing its three tasks. Log every intervention with
   `./oplog.sh <root> intervention <slot> <category> "<note>"`. Categories:
   `unstick`, `answer`, `resolve`, `restart`, `env`, `other`. Nudge with a
   fixed phrase and no hints: "Continue with your tasks until all three have
   landed." Stop an agent after 60 minutes.
5. **End.** When all four agents have stopped, run `./oplog.sh <root> arm_end`.
6. **Merge step (A and B).** Stop `serve` with ctrl-c, then run
   `python3 <root>/bin/merge_queue.py --arm-root <root> drain` to judge
   anything still queued. Arm C needs no merge step: promotion is the merge.
7. **Collect telemetry.** For each slot, put Chau7 `run_get` JSON in
   `telemetry/<slot>/<run>.run.json` and `run_transcript` JSON in
   `telemetry/<slot>/<run>.transcript.json`. Also copy the agent's own
   session log into `telemetry/<slot>/native/`: for Claude Code,
   `~/.claude/projects/<dir-slug>/<sessionID>.jsonl` (the sessionID is in
   `run_get`); for Codex, `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`.
   Chau7 currently reports `tokenUsageState: "missing"` for most TUI runs,
   and `turnCount` counts prompts rather than model calls, so the native logs
   are the token source.
8. **Score.** Run
   `python3 score.py --arm-root <root> --telemetry <root>/telemetry`. It
   writes `<root>/score.json` and prints the headline measures.
9. **Clean up.** Close the four tabs (`tab_close`). For arm C:
   `aethyme broker status --json`, then `aethyme broker finish --session <id>`
   for any session still open in `<root>/repo`, then `aethyme broker gc`.
   Keep the arm root until the scores are compared, then `rm -rf <root>`.
   Arms A and B leave nothing outside their root, apart from Codex-managed
   worktrees: run `git -C <root>/repo worktree list` before removing.

### Measures (`score.json` → `headline`)

| Measure | How |
| --- | --- |
| `wall_time_s` | `arm_start` to `arm_end` from the operator log; if absent, the telemetry window. |
| `tasks_passing_hidden`, `overlaps_resolved` | The hidden checks run on the final landing ref. |
| `final_main_green` | The visible suite on the final landing ref. |
| `conflicts_caught_before_merge` | A and B: queue verdicts `conflict` and `ci_failed`. C: `broker submit` exits 3 (refused) and 4 (gate failed on the merged tree). |
| `broken_merges_reaching_main` | Replays every state of the landing ref (its reflog) and counts green-to-red steps. `red_states` counts every red state. |
| `operator_interventions` | The intervention lines in the operator log. |
| `agent_turns`, `tokens_total` | Model calls and tokens per slot, from the native session logs (Claude usage is deduplicated by message id; Codex uses the last cumulative `token_count`). |

## Cost and time estimate

Each agent does three tasks of about 10 to 40 changed lines each, plus the
landing loop. Expect 30 to 80 model calls per agent and 1.5M to 5M tokens per
agent, mostly cache reads. That is about 6M to 20M tokens per arm-run.

- **Cost.** About $10 to $40 per arm-run on API pricing for the two Claude
  slots, with the Codex slots on the configured subscription.
- **Wall time.** 30 to 60 minutes of agent time per arm-run, plus 15 minutes
  of setup, collection and scoring.
- **Full plan.** 3 arms × 2 repetitions = 6 arm-runs: roughly 5 to 8
  coordinator hours and $60 to $240. One repetition halves both.

## Threats to validity

- **Model nondeterminism.** Twelve tasks and three overlaps per run are few,
  and whether the semantic overlap fires depends on timing. Run 2
  repetitions per arm if the budget allows. Report every run, not the best
  one. Treat a difference of one conflict or one broken merge as noise
  unless it repeats.
- **Prompt differences.** The task blocks are generated from the same
  `tasks.yaml` text and are identical across arms. Only the "how to land"
  section differs. Arm C's section names broker behaviour (merged-tree
  gates) that the A and B sections cannot name, which may itself prime
  caution. `prompts/<slot>.md` is kept per run for audit.
- **Operator bias.** The coordinator knows the hypothesis. Use the fixed
  intervention rules and nudge phrase above, log every touch, and do not
  resolve conflicts for an agent: category `resolve` should stay at zero.
- **Contamination.** The globally enabled `aethyme@aethyme` Claude plugin
  and any Aethyme hooks may inject broker guidance into the Claude slots of
  arms A and B. Keep the plugin state identical across arms, record it, and
  check the A and B transcripts for broker commands.
- **Harness asymmetry.** B's Codex slots use Codex's managed worktree, whose
  location and base branch Codex chooses. A's worktrees are created by the
  setup script. The merge queue's feedback (A, B) and the broker's (C) differ
  in wording and latency, and the queue checks every 2 s.
- **Order and host load.** Rotate the arm order between repetitions. Do not
  run two arms at once: they would compete for CPU and API rate limits.
- **Small, fast suite.** Gates cost well under a second here, so arm C's
  gate overhead is understated compared with a real repository.

## Dry run

`python3 selftest.py <scratch>` checks:

- the base suite passes and all 15 hidden checks fail on the base;
- each reference patch passes its own hidden check and no other task's check;
- the integrated reference passes all 15 checks;
- the three overlaps behave as designed under plain Git;
- the three setup scripts produce valid launch plans;
- a scripted arm A run through the real merge queue (t04 is rejected as a
  conflict, and t08 turns main red) scores as expected with synthetic Chau7
  telemetry;
- a synthetic arm C state scores as expected.

It launches no agent and never runs the broker. The arm C `gates.toml` was
checked with `aethyme broker gates validate` (0.8.3), using a scratch
`HOME`.
