# Bug report: SKILL.md leads agents to a deleted CLI subcommand

Last Reviewed: 2026-07-13

Filed: 2026-05-12
Filed by: graph team (Claude)
Filed to: tooling / experience team
Severity: **high** — confounds every Aethyme-condition eval since 2026-05-08
Discovered during: MediaWiki bug-fix-1 3-sample variance study

## TL;DR

Every Aethyme tool invocation across 9 condition-runs in our 3-sample
variance study **failed**. 0/15 successful Aethyme calls. The agents
in `explore`, `leverage`, and `task-conditioned` conditions all read
`SKILL.md`, then construct invocations like:

```bash
.venv/bin/python -m src.cli explore --repo "..." --request "..."
```

which produces:

```
Exit code 2
Error: No such command 'explore'.
```

`python -m src.cli explore` was **hard-deleted on 2026-05-08** as part
of the Python-explore retirement (see
[`eval-tuning-rejected.md`](./eval-tuning-rejected.md) and the
2026-05-08 native-explore parity-gap closure work). The deployed
SKILL.md still teaches a dual-pattern that leads agents to construct
this invalid command, and they fall back to plain `grep` for the
rest of their session.

**Impact:** every Aethyme-condition eval result since 2026-05-08
is confounded. We have been measuring "agent attempts broken Aethyme
call, then falls back to grep" — NOT "agent uses Aethyme to navigate
code." The discoverability-gap numbers, the regression eval, and the
prior single-sample comparisons all need to be re-interpreted (or
re-run) after the fix.

## Evidence

Full failed invocation captured from the leverage condition (sample
`b08ed05b-a587-...`, 2026-05-12 13:38):

**Command the agent constructed:**

```bash
cd "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme" \
  && .venv/bin/python -m src.cli explore \
       --repo "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme" \
       --request "Bug T419918: viewing a diff/revision on a watchlisted page marks all revisions as seen instead of only the viewed one. Find files related to watchlist revision marking, diff viewing, and tracking which revisions are seen." \
       --detail standard --format answer-json 2>&1
```

**Result:**

```
Exit code 2
Usage: python -m src.cli [OPTIONS] COMMAND [ARGS]...
Try 'python -m src.cli --help' for help.

Error: No such command 'explore'.
```

The agent then made 26 more bash commands — all standard
`grep`/`find`/`ls`, zero retries of Aethyme.

## Frequency

Across the three MediaWiki bug-fix-1 samples on 2026-05-12, every
Aethyme condition attempted Aethyme and every attempt failed:

| Condition | Aethyme attempts (3 samples) | Successful |
|---|---|---|
| Explore | 9 (some sessions retried) | **0** |
| Leverage | 5 | **0** |
| Task-Conditioned | 5 | **0** |

Failure modes observed: exit 127 (file not found), exit 2 (click
"no such command"), exit 1 (general error). The common thread:
agents construct invalid invocations and the CLI rejects them.

## Root cause

The deployed SKILL.md at
`.codex/skills/aethyme/SKILL.md` (and `.claude/skills/aethyme/SKILL.md`,
deployed from `packages/aethyme/skills/aethyme/SKILL.md`) presents
**two coexisting invocation patterns**:

```bash
# Pattern A — native Rust binary (for `explore` ONLY)
AETHYME_BIN="$AETHYME_ROOT/rust/target/release/aethyme"
"$AETHYME_BIN" explore --repo "$REPO" ...

# Pattern B — Python CLI (for graph/task/intents/etc.)
AETHYME_PY="$AETHYME_ROOT/.venv/bin/python"
(cd "$AETHYME_ROOT" && "$AETHYME_PY" -m src.cli graph callers ...)
(cd "$AETHYME_ROOT" && "$AETHYME_PY" -m src.cli task scope ...)
(cd "$AETHYME_ROOT" && "$AETHYME_PY" -m src.cli intents ...)
```

Agents naturally pattern-mismatch and construct
`"$AETHYME_PY" -m src.cli explore` — which used to work, but was
removed in commit history under "feat: native explore parity-gap
closure + soft-retire Python explore."

This is **exactly the failure mode that
[`cross-process-consumers.md`](./cross-process-consumers.md) was
created to prevent.** The deletion of the Python `explore` subcommand
correctly went through the soft-retire → hard-delete contract for
**code** consumers (deployed wrappers updated, tests cleaned up).
But SKILL.md is a **text consumer** whose stale references aren't
caught by the contract-check CI workflow (`07ec796`) — that workflow
scans diff content, not the textual surface of skill files.

## Why we didn't catch this until now

Three reasons compound:

1. **Silent failure**: exit 127 ("no such file") and exit 2
   ("Error: No such command") are not surfaced to the agent's
   strategy in any visible way. The tool call returns, the agent
   reads the error, and moves on to its fallback strategy. Nothing
   in our eval pipeline flags "Aethyme invocations are all failing."
2. **Fallback masks the issue**: agents have plenty of grep/Read
   tools to fall back on. The bug-fix-1 task is doable without
   Aethyme. So the eval *produces JSON output, scores cleanly, and
   reports*, with no obvious smoke.
3. **The eval semantics assume Aethyme works**: when leverage costs
   more than control, we attributed it to "Aethyme overhead." It's
   actually "wasted tool-call overhead from failed Aethyme attempts."
   Indistinguishable without inspecting the JSONL transcripts.

## Impact on prior findings

Every Aethyme-condition eval since 2026-05-08 needs re-interpretation:

| Prior finding | Re-interpretation |
|---|---|
| GRC bug-fix discoverability gap: −20.6% (cost) | Probably "agent attempted broken Aethyme then grep-explored" cost premium |
| MediaWiki bug-fix-1 prior recall 1/4 for Aethyme | Same — agents never actually used Aethyme |
| 3-sample variance study: gap signs flip | Confirmed: noise on top of a broken pipeline |
| "Aethyme-quality-deficit" of −10 points on explore/leverage | NOT a graph problem — a tooling problem (this bug) |

None of the eval results are "wrong" in the sense of being
mis-collected — but the *interpretation* that Aethyme provides
some specific cost/quality tradeoff is unsupported. We've been
measuring the cost of a tool the agents can't actually run.

The graph-side commits this session (`35ac25c`, `7a01c32`,
`fe15650`, `bdaf014`) all stand on their own merits as
construction/ranking improvements. They produce measurably better
graph data and better anchor surfaces, verifiable independently.
But their downstream impact on agent behavior **cannot be measured
through bug-fix-1 evals until this tooling bug is fixed.**

## Reproduction

1. Deploy the current Aethyme skill into a Playground repo:
   ```bash
   .venv/bin/python -m src.cli enhance deploy \
     --repo "/path/to/Playground/Mediawiki/Mediawiki - Aethyme" --force
   ```

2. Launch a Claude Code session against that repo (any model):
   ```bash
   cd "/path/to/Playground/Mediawiki/Mediawiki - Aethyme"
   claude --dangerously-skip-permissions --model haiku
   ```

3. Issue a prompt that mentions Aethyme:
   ```
   Aethyme is available in this repository. See `.codex/skills/aethyme/SKILL.md`
   for usage. Find files implementing watchlist seen-marking.
   ```

4. Inspect the agent's tool calls. The agent will read SKILL.md
   and (often) attempt `python -m src.cli explore` — exit 2 with
   "No such command 'explore'."

Empirically observed: this happens in 100% of MediaWiki bug-fix-1
runs (n=9 across 3 samples × 3 Aethyme conditions).

## Recommended fixes (ranked by effort)

### Option 1: Add an explicit "DO NOT" warning to SKILL.md *(smallest fix)*

Insert near the top of the "Setup" section:

```markdown
> **Important:** `python -m src.cli explore` is NOT a valid command.
> Use the native binary `"$AETHYME_BIN" explore ...` only. The Python
> CLI handles `graph`, `task`, `intents`, `facts`, and `analyze` —
> but NOT `explore`.
```

Estimated effort: 10 minutes. Effectiveness: probably partial —
agents that don't carefully read the warning will still construct
the invalid invocation. Pre-LLM-instruction-following standards
would say this is sufficient; in practice, ~50% of agent runs would
still fail because the dual-pattern remains.

### Option 2: Unify CLI surface so `aethyme <subcommand>` covers all commands *(medium fix)*

Make the native binary route every Aethyme subcommand:

```bash
aethyme explore ...       # → native Rust path (today)
aethyme graph callers ... # → currently $AETHYME_PY -m src.cli graph callers
aethyme task scope ...    # → currently $AETHYME_PY -m src.cli task scope
aethyme intents ...       # → currently $AETHYME_PY -m src.cli intents
```

Then SKILL.md becomes a single-pattern document with no opportunity
for cross-product mismatching. The Rust binary can either implement
the missing subcommands natively or shell out to the Python CLI
internally (transparently to the agent).

Estimated effort: 1-2 days. Effectiveness: high — eliminates the
class of error entirely. Side benefit: simplifies agent mental model
of "how do I invoke Aethyme."

### Option 3: Add a strict-mode flag to the Python CLI that intercepts removed subcommands *(small fix, complementary)*

When `python -m src.cli explore` is invoked, instead of click's
generic "No such command", emit a specific message:

```
Error: 'explore' was removed from the Python CLI on 2026-05-08
in favor of the native binary. Use:

  "$AETHYME_ROOT/rust/target/release/aethyme" explore ...

See SKILL.md or eval-tuning-rejected.md for context.
```

Estimated effort: 30 minutes. Effectiveness: moderate — agents that
get this clear error message can re-attempt with the correct
command, recovering from the failed first attempt. Doesn't prevent
the failed first attempt, but converts a silent dead-end into a
recoverable hint.

### Option 4: Add SKILL.md content validation to the contract-check CI *(structural fix)*

Extend the cross-process-consumers check (`scripts/check-cross-process-contract.py`,
commit `07ec796`) to also scan SKILL.md and AGENTS.md for references
to symbols that no longer exist in the source tree. Catches the next
deletion-without-doc-update before it ships.

Estimated effort: half day. Effectiveness: prevents recurrence.

**Recommended combination:** Option 1 immediately (10-minute
unblock), then Option 3 (clearer error message for agents that
still construct the bad invocation), then Option 4 (prevent
recurrence). Option 2 is a larger architectural improvement worth
considering separately.

## How to verify the fix worked

Run one MediaWiki bug-fix-1 eval after the fix lands:

```bash
.venv/bin/python -m src.cli eval run \
  --eval-type bug-fix-1 --target mediawiki --model haiku \
  --json-output
```

…and launch the 5 conditions as usual. Then check the leverage
condition's JSONL transcript for Aethyme invocation exit codes:

```bash
# From the session JSONL for the leverage condition:
.venv/bin/python -c "
import json, sys
for line in open(sys.argv[1]):
    obj = json.loads(line)
    if obj.get('type') == 'user':
        for c in obj.get('message', {}).get('content', []):
            if isinstance(c, dict) and c.get('type') == 'tool_result':
                txt = c.get('content', '')
                if isinstance(txt, list):
                    txt = ''.join(p.get('text','') for p in txt if isinstance(p, dict))
                if 'src.cli' in txt or 'aethyme explore' in txt:
                    print(txt[:200])
" "$HOME/.claude/projects/<session>/<sid>.jsonl"
```

Verify: every Aethyme tool_result shows exit 0 (or a successful
JSON response), not exit 2 / 127. If yes, the fix is in.

Bonus check: leverage cost should drop substantially. The "wasted
turn on failed invocation" cost was costing us roughly 1 grep-equivalent
turn per Aethyme attempt — across 9 runs that's ~9 wasted turns of
context. With Aethyme working, leverage may finally show real
cost/quality differences vs explore that reflect Aethyme's actual
impact.

## Related docs

- [`cross-process-consumers.md`](./cross-process-consumers.md) —
  the inventory this bug should have been caught by
- [`eval-tuning-rejected.md`](./eval-tuning-rejected.md) — the
  2026-05-08 hard-delete is recorded there
- [`docs/reports/evals/`](../reports/evals/) — generated eval
  reports, including the MediaWiki bug-fix-1 samples that surfaced the deficit
- `.github/workflows/cross-process-contract.yml` — the CI gate
  that Option 4 would extend
