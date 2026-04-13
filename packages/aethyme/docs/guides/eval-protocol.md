# Eval Protocol — Aethyme Navigation Benchmarks

Last Updated: 2026-04-13

## Runtime Flow

The runtime protocol is split into four distinct layers:

1. **Setup Playground**
   - Clone or refresh the Control and Aethyme repos for a target.
   - Deploy the Aethyme skill into the Aethyme repo.
   - Build the Aethyme graph index.
2. **Prepare Target**
   - Perform lightweight readiness checks only.
   - Persist a preparation snapshot under `eval-runs/preparations/`.
   - This step must not generate prompts or launch agents.
3. **Generate Plan**
   - Build the dry execution plan: conditions, paths, model/backend, prompts to materialize, run dir.
   - This is orchestration metadata only.
4. **Run Evaluation**
   - Materialize eval inputs.
   - Create tabs.
   - Launch agents.
   - Collect outputs.
   - Score and finalize artifacts.

`Prepare Target` is intentionally light. Heavy work like leverage prompt enrichment belongs to `Run Evaluation` under the explicit `build inputs` phase, not to repository preparation.

## Core Rules

### 1. Playground Only

**Evaluations target only Playground repositories.** Never execute eval agents against the Aethyme repo itself — it conflates the tool with the subject.

### 2. No Eval-Driven Tool Changes

**It is absolutely forbidden to modify Aethyme's tools, engine, pipeline, or skills to improve eval scores.** This is the single most important integrity rule for the project.

Evals measure the generic system. The generic system must never be shaped to match eval expectations. The causal arrow is:

```
Generic tool improvement → eval scores change (observed)
```

Never:

```
Eval score is low → tweak tool to raise that score
```

Examples of **forbidden** changes:
- Adding task-type-specific code paths in the engine to handle explain-repo differently from other tasks (e.g., injecting `overview_docs` into the file content reader only for explain-repo)
- Adding special-case logic in anchor resolution, scope building, or file selection that targets known eval scenarios
- Tuning budget defaults, priority orderings, or heuristics based on what a specific eval run scored poorly on
- Modifying scoring rubrics to be more lenient toward Aethyme conditions
- Changing skill prompts to hint at eval output formats
- Adding post-processing in the pipeline that reformats output to match scorer expectations

Examples of **allowed** changes:
- Fixing a bug where the engine fails to read files it should be reading (generic correctness)
- Improving anchor resolution quality across all task types based on structural analysis
- Adding a new generic capability (like file content in context packs) that benefits any task
- Fixing the scorer itself when it has genuine bugs (e.g., path normalization)
- Improving graph construction (better edge detection, better area inference) — these are structural improvements to the map, not eval accommodations

**The test:** Before making any change, ask: "Would I make this exact change if the eval didn't exist?" If the answer is no, or if the change only makes sense in the context of improving a specific eval metric, do not make it.

**Why this matters:** Aethyme's value proposition is that a generic structural graph improves agent navigation on ANY repository and ANY task. If the tools are tuned to score well on specific evals, the product loses its generality claim. Eval scores that come from tool-tweaking are vanity metrics — they prove nothing about real-world value. A system that scores 70 honestly is worth more than one that scores 95 through overfitting.

**When evals reveal genuine problems:** If an eval shows that Aethyme-equipped agents underperform vanilla agents, the correct response is to investigate whether there is a generic deficiency in the system (poor anchor quality, missing graph edges, excessive irrelevant context, skill instructions that waste turns). Fix the generic problem. Then re-run the eval to see if the generic fix helped. The eval is a diagnostic, not a target.

## Local Eval Storage

Eval artifacts are stored locally under `packages/aethyme/eval-runs/` (gitignored). Each run creates a timestamped directory. **Every piece of data that exists at eval time must be stored.** Any run must be fully investigable retroactively.

```
eval-runs/
  YYYYMMDD-HHMMSS-<slug>-<eval-type>/
    metadata.json              # timestamp, aethyme commit, repo path, eval type, conditions
    complete-result.json       # FULL result dict — single source of truth
    report.md                  # rendered markdown report
    artifacts/                 # inputs and reference data
      control-cto-off-prompt.txt
      control-cto-on-prompt.txt
      explore-prompt.txt
      leverage-prompt.txt
      output-schema.json
      scoring-rubric.json
      reference-output.json
      navigation-context.json
      pack.json
      task-spec.json
      challenge.json
      signals.json
    conditions/                # per-condition outputs
      control-cto-off/
        result.json            # structured output from agent
        assessment.json        # scoring assessment
        run-record.json        # full run data (tokens, timing, exit code, etc.)
        run-metadata.json      # quick-access tokens/duration/exit_code
        raw-stdout.txt         # complete agent stdout
        raw-stderr.txt         # complete agent stderr
        tool-calls.json        # tool calls from run data
      control-cto-on/
        ...
      explore/
        ...
      leverage/
        ...
    chau7/                     # Chau7 telemetry (MANDATORY for every condition)
      control-cto-off/
        metadata.json          # run_id, session_id, counts
        run-id.txt
        session-id.txt
        transcript.json        # full per-turn conversation transcript
        tool-calls.json        # detailed tool call data from Chau7
        tab-output.txt         # raw terminal output
      control-cto-on/
        ...
      explore/
        ...
      leverage/
        ...
```

### Storage Functions

| Function | Purpose | When to call |
|----------|---------|--------------|
| `create_eval_run_dir()` | Create the timestamped directory with all subdirs | Step 1 (artifact generation) |
| `store_condition_raw()` | Store a condition's raw output (stdout, stderr, structured output, tokens, duration) | Immediately after each condition completes (Step 4) |
| `store_condition_chau7()` | Store Chau7 run_id, session_id, transcript, tool_calls, tab_output | After each condition completes (Step 4.5) |
| `write_eval_run_artifacts()` | Write all shared and per-condition artifacts from a result dict | Step 5 (after scoring) |
| `finalize_eval_run()` | Write complete-result.json, final report.md, and all artifacts | Step 6 (final step) |

### Storage Contract

Every eval run **must** have:
1. `complete-result.json` — the entire result dict with all data that existed at eval time
2. Per-condition `result.json` + `assessment.json` — structured output and scores
3. Per-condition `raw-stdout.txt` — the complete agent output before parsing
4. Per-condition Chau7 `transcript.json` + `tool-calls.json` — full turn-by-turn data
5. All shared artifacts (prompts, schema, reference, pack, navigation context)

If any of these are missing, the run is incomplete and must be re-captured or marked as partial in metadata.

Condition slugs: `control-cto-off`, `control-cto-on`, `explore`, `leverage`.

### Data Collection Checklist

Run this checklist **after every condition completes**, before moving to the next phase. Skipping any item produces invalid data.

#### 1. Verify prompt was received
```
Check the first `type: "user"` message in the session JSONL.
Confirm it contains the expected prompt text.
If the condition is leverage, confirm the navigation context is present.
```
**Why:** prompt delivery is only valid after `tab_send_input(...)` is followed by `tab_submit_prompt()`. Do not assume the pasted text was actually accepted by Claude until the first user message in the session JSONL matches the expected prompt.

#### 2. Check for sub-agent sessions
```
Session JSONL path: ~/.claude/projects/<encoded-repo-path>/<session-id>.jsonl
Sub-agent path:     ~/.claude/projects/<encoded-repo-path>/<session-id>/subagents/*.jsonl
```
**Why:** Claude Code (especially Haiku) frequently delegates to sub-agents via the `Agent` tool. The parent session may show 5 turns and $0.14, but the sub-agent did 56 turns and $0.55. Always sum parent + sub-agent tokens for the true cost.

#### 3. Collect token counts from ALL session files
```python
# For each condition, sum across parent + all sub-agents:
total_input    = parent.input + sum(sub.input for sub in subagents)
total_output   = parent.output + sum(sub.output for sub in subagents)
total_cache_r  = parent.cache_read + sum(sub.cache_read for sub in subagents)
total_cache_c  = parent.cache_create + sum(sub.cache_create for sub in subagents)
total_turns    = parent.turns + sum(sub.turns for sub in subagents)
total_tools    = parent.tools + sum(sub.tools for sub in subagents)
# Exclude the parent's "Agent" tool call from tool count (it's delegation, not work)
```

#### 4. Verify CTO state per tab
```
tab_status(tab_id) → check cto_active and cto_override fields.
control-cto-off must show cto_override: "forceOff"
all others must show cto_active: true (if CTO is globally enabled)
```
**Why:** CTO was found to be globally disabled during one eval run, making all conditions identical. Always verify CTO state before drawing CTO conclusions.

#### 5. Map tab IDs to session files
```
Record which tab_id corresponds to which condition BEFORE launching.
Launch Claude with an explicit `--session-id <uuid>` per condition.
Compute the exact JSONL path:
~/.claude/projects/<encoded-repo-path>/<session-id>.jsonl
Verify the first user message matches the condition prompt.
```
**Why:** `tab_status.ai_session_id` is not reliable enough for attribution, and same-repo conditions can otherwise be ambiguous. Explicit session IDs remove guesswork and make collection deterministic.

### Known Limitations

- **`tab_status.ai_session_id` is unreliable for condition attribution.** It can lag, point at an older session, or repeat across tabs in the same repo pair. Use the explicit `--session-id` passed at launch and the expected JSONL path instead.
- **`runtime_session_create` with `backend="claude"` fails** with `--print-session-id` error on current Claude Code versions. Use `tab_create` + `tab_exec` + `tab_send_input` + `tab_submit_prompt` instead.
- **Sub-agent JSONL files** are stored in `<session-id>/subagents/` subdirectory, not alongside the parent. Token extraction code must recurse into this directory.
- **Haiku strategy variance is high.** The same prompt can produce 5-turn or 70-turn sessions depending on whether the model delegates to a sub-agent. Multiple runs per condition are needed for stable averages.
- **The eval UI server uses file-first output capture.** If the prompt successfully makes the agent write `.aethyme-eval-output-<condition>.md`, prefer that file. Fall back to PTY log capture only when the file is missing.

## Playground Repositories

Target repos are registered in `src/eval/targets.py` (canonical source). Run `python -m src.cli eval targets` to list and validate.

| Target | Display | Control | Aethyme | Notes |
|--------|---------|---------|---------|-------|
| `grc` | GRC | `Playground/GRC/Playground Control` | `Playground/GRC/Playground Aethyme` | TypeScript monorepo |
| `mediawiki` | MediaWiki | `Playground/Mediawiki/Mediawiki - Control` | `Playground/Mediawiki/Mediawiki - Aethyme` | PHP monolith (~12.5K files) |

Each target is a pair: a vanilla **control** repo (no `.codex/skills/`) and an **aethyme** repo with the skill deployed. To add a new playground: clone a repo into both slots, deploy the Aethyme skill to the aethyme copy, and add an entry to `TARGETS` in `targets.py`.

### Isolated Benchmark Repos (Bug-Fix and Stateful Evals)

For evals where agents modify the repo (e.g. bug-fix), each condition must operate on its own pristine clone. Without isolation, the first condition to fix the bug leaves it fixed for subsequent conditions, invalidating the comparison.

```
/tmp/benchmark-run-001/
  control-cto-off/     # git clone --local, no .codex/ skill
  control-cto-on/      # git clone --local, no .codex/ skill
  explore/             # git clone --local + .codex/skills/aethyme/SKILL.md
  leverage/            # git clone --local + .codex/skills/aethyme/SKILL.md
```

**One-command setup:**

```bash
cd packages/aethyme
python -m src.cli eval bug-fix prepare \
  --source "/path/to/Playground" \
  --dest "/tmp/benchmark-run-001"
```

This creates 4 clones (via `git clone --local` for speed), deploys the Aethyme skill to explore/leverage, plants the bug in each, and writes all prompt/schema/reference artifacts to `/tmp/`.

**Repo-only setup** (no bug planting, useful for other eval types):

```bash
python -m src.cli eval setup-repos \
  --source "/path/to/Playground" \
  --dest "/tmp/benchmark-run-001"
```

Control conditions get no `.codex/` directory. Explore and leverage get `.codex/skills/aethyme/SKILL.md` with the Aethyme CLI path stamped in.

## 4-Condition Design

Every eval compares four conditions to isolate the value of graph-derived navigation context and terminal optimization:

| Condition | What the agent receives | Nav context file | Graph CLI in prompt | Chau7 CTO | What it tests |
|-----------|------------------------|-----------------|---------------------|-----------|---------------|
| **Control (CTO off)** | Task + repo path only | No | No | forceOff | Raw LLM exploration, uncompressed terminal output |
| **Control (CTO on)** | Task + repo path only | No | No | default | Raw LLM exploration with terminal compression |
| **Explore** | Task + repo path + CLI commands | No | Yes | default | Whether graph tools alone help |
| **Leverage** | Task + pre-computed navigation context file | Yes | Yes (in context) | default | Whether pre-computed graph analysis adds value |

The two control conditions isolate the effect of Chau7's Command Token Optimization (CTO) on baseline agent behavior. CTO compresses terminal output to reduce token usage — disabling it gives a true uncompressed baseline.

## Eval Types

### explain-repo

Task: "Explain this repo" — produces a structured overview with code areas, entrypoints, docs, configs, languages, risks, navigation order, and evidence.

#### Exact Prompts

**Control prompt (CTO off and CTO on — same prompt, different CTO setting):**

```
Task: Explain this repo
Repository path: <PLAYGROUND_PATH>
Explore the repository directly and produce a structured explanation.
```

**Explore prompt:**

```
Task: Explain this repo
Repository path: <PLAYGROUND_PATH>
Explore the repository and produce a structured explanation.

You have access to the following graph navigation commands.
Use them via Bash to explore the repository graph:

  cd <AETHYME_PATH> && <AETHYME_VENV>/bin/python -m src.cli repo inspect '<PLAYGROUND_PATH>' --json-output
  cd <AETHYME_PATH> && <AETHYME_VENV>/bin/python -m src.cli graph overview '<PLAYGROUND_PATH>' --json-output
  cd <AETHYME_PATH> && <AETHYME_VENV>/bin/python -m src.cli graph expand '<PLAYGROUND_PATH>' <anchor-id> --json-output

Return only the required structured output.
```

**Leverage prompt:**

```
Task: Explain this repo
Read the navigation context file at /tmp/aethyme-eval-navigation-context.json for pre-computed anchors, scope, and CLI commands.
Use it as your primary navigation layer. Return only the required structured output.
```

The navigation context file contains: anchors (key files/folders to start from), scope (in-scope files/symbols/areas, out-of-scope areas, risk flags), navigation order, and the same CLI commands available to Explore.

#### Output Schema

All conditions must produce JSON conforming to this schema:

```json
{
  "repo_summary": "string",
  "code_areas": ["string"],
  "reference_areas": ["string"],
  "entrypoints": ["string"],
  "important_docs": ["string"],
  "key_configs": ["string"],
  "key_languages": ["string"],
  "high_risk_areas": ["string"],
  "navigation_order": ["string"],
  "representative_code_files": ["string"],
  "representative_docs": ["string"],
  "evidence": ["string"]
}
```

### navigation-ctf

Task: Directed graph navigation challenge — find the config, entrypoint, and management area linked by graph relations. The challenge is auto-generated from real graph edges: the engine finds a config node with both `entrypoint_for` and `configures` edges, then asks the agent to discover those same targets.

#### Exact Prompts

The task text is generated per-repo. Example for the Aethyme Playground:

**Control prompt (CTO off and CTO on — same prompt, different CTO setting):**

```
Task: Find the manifest that manages the main code entrypoint in the <AREA> area, identify the entrypoint file it controls, and name the top-level area that owns both.
Repository path: <PLAYGROUND_PATH>
Explore the repository directly and produce a structured explanation.
```

**Explore prompt:**

```
Task: Find the manifest that manages the main code entrypoint in the <AREA> area, identify the entrypoint file it controls, and name the top-level area that owns both.
Repository path: <PLAYGROUND_PATH>
Explore the repository and produce a structured explanation.

You have access to the following graph navigation commands.
Use them via Bash to explore the repository graph:

  cd <AETHYME_PATH> && <AETHYME_VENV>/bin/python -m src.cli task anchors --repo '<PLAYGROUND_PATH>' --task <task> --json-output
  cd <AETHYME_PATH> && <AETHYME_VENV>/bin/python -m src.cli task scope --repo '<PLAYGROUND_PATH>' --task <task> --json-output
  cd <AETHYME_PATH> && <AETHYME_VENV>/bin/python -m src.cli graph configs '<PLAYGROUND_PATH>' <area> --json-output
  cd <AETHYME_PATH> && <AETHYME_VENV>/bin/python -m src.cli graph expand '<PLAYGROUND_PATH>' <anchor-id> --json-output

Return only the required structured output.
```

**Leverage prompt:**

```
Task: Find the manifest that manages the main code entrypoint in the <AREA> area, identify the entrypoint file it controls, and name the top-level area that owns both.
Read the navigation context file at /tmp/aethyme-eval-ctf-navigation-context.json for pre-computed anchors, scope, and CLI commands.
Use it as your primary navigation layer. Return only the required structured output.
```

#### Output Schema

All conditions must produce JSON conforming to this schema:

```json
{
  "config_target": { "path": "string", "why": "string" },
  "code_target": { "path": "string", "why": "string" },
  "management_area": { "name": "string", "why": "string" },
  "relationship_chain": [{ "from": "string", "to": "string", "relation": "string" }],
  "rejected_candidates": [{ "path": "string", "reason": "string" }],
  "confidence": "string"
}
```

#### Scoring Rubric

| Field | Weight | Method |
|-------|--------|--------|
| config_target | 30 | Exact path match |
| code_target | 30 | Exact path match |
| management_area | 20 | Exact name match |
| relationship_chain | 20 | Set match on `(from, to, relation)` triples |

#### Qualitative Review

After scoring, produce a human-readable report per condition with:

- **Config Target** — which manifest the agent chose and why
- **Code Target** — which entrypoint file it identified and why
- **Management Area** — which area it named
- **Relationship Chain** — the edges the agent traced
- **Rejected Candidates** — what the agent considered and ruled out
- **Confidence** — the agent's self-assessed confidence

Then produce a qualitative comparison table:

| Dimension | Control (CTO off) | Control (CTO on) | Explore | Leverage |
|-----------|-------------------|------------------|---------|----------|
| Target accuracy | ... | ... | ... | ... |
| Reasoning quality | ... | ... | ... | ... |
| Path format | ... | ... | ... | ... |
| Relationship chain | ... | ... | ... | ... |
| Token Efficiency | ... | ... | ... | ... |
| Self-awareness | ... | ... | ... | ... |

## Execution Method

### Orchestrator-Based Execution (Preferred)

The orchestrator generates a complete, deterministic run plan that Claude executes mechanically via Chau7 MCP. No runtime decisions needed.

**Generate a plan:**

```bash
cd packages/aethyme
python -m src.cli eval run --eval-type bug-fix --target grc --model haiku
python -m src.cli eval run --eval-type explain-repo --target mediawiki --model sonnet --json-output
```

The `--json-output` flag emits the full plan dict with 8 phases:

0. **prepare** — repository readiness checks and readiness snapshot contract
1. **build-inputs** — materialize prompts, schema, reference, nav context, bug-fix repo prep
2. **launch** — create 4 Chau7 tabs, start the backend in each tab, then submit the prompt
3. **monitor** — poll until all agents complete
4. **collect** — gather structured output, telemetry, transcripts per condition
5. **score** — call `assemble_bug_fix_result()` or equivalent scorer
6. **report** — call `finalize_eval_run()` + `print_scorecard()` (never hand-write)
7. **cleanup** — close all sessions and tabs

Each phase contains all parameters pre-computed. Claude reads the plan and executes each step via Chau7 MCP tools (`tab_create`, `tab_exec`, `tab_send_input`, `tab_submit_prompt`, `tab_set_cto`, `tab_status`, `tab_output`, etc.).

**Supported models:**

| Model | Provider | Backend | Launch Method |
|-------|----------|---------|---------------|
| haiku | Anthropic | claude | `tab_create` + `tab_exec` + `tab_send_input` + `tab_submit_prompt` |
| sonnet | Anthropic | claude | `tab_create` + `tab_exec` + `tab_send_input` + `tab_submit_prompt` |
| opus | Anthropic | claude | `tab_create` + `tab_exec` + `tab_send_input` + `tab_submit_prompt` |
| gpt-5.4 | OpenAI | codex | `tab_create` + `tab_exec` with `codex exec` |

### Manual Execution (Fallback)

For cases where the orchestrator doesn't cover a specific need, the manual step-by-step process is below.

### Step-by-Step

#### 0. Verify Aethyme Tooling is Up to Date

Before every eval run, confirm that the Aethyme repo itself is current. A stale engine binary, outdated eval code, or old scoring logic will produce results that aren't comparable to future runs.

```bash
cd "/Users/christophehenner/Downloads/Repositories/Aethyme"
git fetch origin
git log --oneline -1 HEAD
git log --oneline -1 origin/main
```

If HEAD is behind `origin/main`, pull before proceeding:

```bash
git pull origin main
```

Then verify the engine binary is built from the latest source:

```bash
cd packages/aethyme
# Check if release binary exists and is newer than source
ls -la rust/target/release/aethyme-engine-cli
# Rebuild if needed
cd rust && cargo build --release
```

Record the Aethyme commit hash alongside eval results for reproducibility.

#### 1. Generate Artifacts

From `packages/aethyme/`:

```python
from pathlib import Path
from src.eval.explain_repo import run_explain_repo_evaluation
from src.eval.report import create_eval_run_dir
import json

PLAYGROUND = Path("/Users/christophehenner/Downloads/Repositories/Aethyme Playground")

# Create persistent run directory
run_dir = create_eval_run_dir(PLAYGROUND, "explain-repo")

# Generates artifacts only (prompts, schemas, reference, context).
# Does NOT execute agent sessions — those are launched via Chau7 MCP (step 2-3).
result = run_explain_repo_evaluation(PLAYGROUND)

# Write prompt files (also to /tmp/ for agent access)
# Both control conditions use the same prompt — CTO setting is configured on the Chau7 tab
Path('/tmp/aethyme-eval-control-cto-off-prompt.txt').write_text(result['control']['prompt'])
Path('/tmp/aethyme-eval-control-cto-on-prompt.txt').write_text(result['control']['prompt'])
Path('/tmp/aethyme-eval-explore-prompt.txt').write_text(result['explore']['prompt'])
Path('/tmp/aethyme-eval-leverage-prompt.txt').write_text(result['leverage']['prompt'])
Path('/tmp/aethyme-eval-output-schema.json').write_text(json.dumps(result['output_schema'], indent=2))
Path('/tmp/aethyme-eval-navigation-context.json').write_text(json.dumps(result['navigation_context'], indent=2))
Path('/tmp/aethyme-eval-reference.json').write_text(json.dumps(result['reference_output']))
```

This produces:
- `/tmp/aethyme-eval-control-cto-off-prompt.txt`
- `/tmp/aethyme-eval-control-cto-on-prompt.txt`
- `/tmp/aethyme-eval-explore-prompt.txt`
- `/tmp/aethyme-eval-leverage-prompt.txt`
- `/tmp/aethyme-eval-output-schema.json`
- `/tmp/aethyme-eval-navigation-context.json`
- `/tmp/aethyme-eval-reference.json`
- `eval-runs/<timestamp>-<slug>-explain-repo/` with `metadata.json`

#### 2. Create Agent Sessions

Open one terminal session per condition, each with the working directory set to the playground repo path.

With Chau7 MCP:
```
tab_create(directory="/Users/christophehenner/Downloads/Repositories/Aethyme Playground")  # x4
```

Then configure CTO per condition:
```
tab_set_cto(tab_id=<control-cto-off-tab>, override="forceOff")
# The other 3 tabs use default CTO (no override needed)
```

#### 3. Launch Agents

In each session, run the agent with the corresponding prompt. Example with Codex:

```bash
codex exec \
  --skip-git-repo-check \
  --sandbox workspace-write \
  --output-schema /tmp/aethyme-eval-output-schema.json \
  --output-last-message /tmp/aethyme-eval-<CONDITION>-result.json \
  "$(cat /tmp/aethyme-eval-<CONDITION>-prompt.txt)"
```

Where `<CONDITION>` is `control-cto-off`, `control-cto-on`, `explore`, or `leverage`.

Both control conditions use the same prompt — only the CTO setting differs. Any agent that can accept a system prompt and produce structured JSON output works. The output must conform to the schema at `/tmp/aethyme-eval-output-schema.json`.

#### 4. Monitor

Record the **wall-clock start time** before launching each agent. Wait for each session to finish, then record the end time:
```
# Before launch: note the time
# After completion:
tab_status(tab_id=<id>)    # "idle" means done
tab_output(tab_id=<id>, lines=15)  # token counts at bottom
```

Compute run time as `end - start` per condition. Report in the scores table as `Xm Ys`.

#### 4.5. Capture Chau7 Telemetry (MANDATORY)

After each agent finishes, **immediately** capture run telemetry. This is not optional — every eval run must have full transcript and tool call data for every condition. Without this data, the run cannot be investigated retroactively.

Get run IDs from `tab_status` or via `session_list` + `run_list`:

```python
from src.eval.report import store_condition_raw, store_condition_chau7

# For each condition tab:
status = tab_status(tab_id=<id>)
run_id = status["active_run"]["id"]  # or find via run_list

# Store raw result immediately
result_json = json.loads(Path(f"/tmp/aethyme-eval-{cond}-result.json").read_text())
tab_out = tab_output(tab_id=<id>, lines=5000)
store_condition_raw(
    run_dir, cond,
    stdout=tab_out,
    structured_output=result_json,
)

# Capture and store Chau7 telemetry
transcript = run_transcript(run_id=run_id)
tool_calls = run_tool_calls(run_id=run_id)
store_condition_chau7(
    run_dir, cond,
    run_id=run_id,
    transcript=transcript,
    tool_calls=tool_calls,
    tab_output=tab_out,
)
```

Repeat for every condition before proceeding to scoring.

#### 5. Collect, Score, and Finalize

```python
import json
from pathlib import Path
from src.eval.scoring import score_explain_repo_output
from src.eval.report import finalize_eval_run

reference = json.loads(Path('/tmp/aethyme-eval-reference.json').read_text())
repo_path_str = str(PLAYGROUND)

for cond in ("control-cto-off", "control-cto-on", "explore", "leverage"):
    candidate = json.loads(Path(f"/tmp/aethyme-eval-{cond}-result.json").read_text())
    assessment = score_explain_repo_output(candidate, reference, repo_path=repo_path_str)
    result[cond]["run"] = result[cond].get("run") or {}
    result[cond]["run"]["structured_output"] = candidate
    result[cond]["assessment"] = assessment
    print(f"{cond}: {assessment['weighted_score']} / {assessment['max_score']}")

# Finalize: writes complete-result.json, all artifacts, and final report.md
finalize_eval_run(run_dir, result, repo_path=PLAYGROUND, eval_type="explain-repo")
```

After `finalize_eval_run`, the run directory contains everything needed to investigate the run at any future point.

#### 6. Qualitative Review

After scoring, read each result file and produce a human-readable report per condition with these sections:

- **Summary** — the agent's `repo_summary`
- **Code Areas** — numbered list with brief description per area
- **Reference Areas** — what the agent identified as non-code reference material
- **Entrypoints** — runtime entry files the agent found
- **Important Docs** — key documentation files
- **Key Configs** — configuration files identified
- **Languages** — programming languages detected
- **High Risk Areas** — areas the agent flagged as dangerous to change
- **Navigation Order** — the agent's recommended reading order
- **Evidence** — key facts and citations the agent used

Then produce a qualitative comparison table:

| Dimension | Control (CTO off) | Control (CTO on) | Explore | Leverage |
|-----------|-------------------|------------------|---------|----------|
| Depth | ... | ... | ... | ... |
| Accuracy | ... | ... | ... | ... |
| Structure | ... | ... | ... | ... |
| Unique Finds | ... | ... | ... | ... |
| Token Efficiency | ... | ... | ... | ... |
| Why Score is Low | ... | ... | ... | ... |

## Results Report Template

Every eval report is generated by `_render_markdown()` in `src/eval/report.py`.
The section order is fixed and non-negotiable. Never hand-write reports — always
use `write_bug_fix_markdown_report()`, `finalize_eval_run()`, or equivalent.

Reports contain these sections **in this exact order**:

1. **Meta** — Date, repository path, eval type, scenario (if applicable), conditions list, Aethyme commit hash.
2. **Model** — Name, provider, backend, reasoning level, permission mode. Without model details, results cannot be compared across runs. Pass model metadata via `model=` in `assemble_bug_fix_result()` or `create_eval_run_dir()`.
3. **Scorecard** — One row per condition with: Score, Cost (USD), Duration, Turns, Input Tokens, Output Tokens, Cache Read Tokens, Cache Create Tokens.

| Condition | Score | Cost | Duration | Turns | Input Tokens | Output Tokens | Cache Read | Cache Create |
|---|---|---|---|---|---|---|---|---|
| Control (CTO off) | 93.83 | $0.161 | 74.8s | 10 | 146 | 2,718 | 542,126 | 74,517 |

4. **Score Breakdown** — Per-component weights and raw values (only rendered when scoring produces component scores, e.g. bug-fix).
5. **Prompts** — Verbatim prompt text for each condition in fenced code blocks. All 4 conditions always shown.
6. **Agent Output** — Structured output JSON for each condition.
7. **Tool Call Analysis** (auto-generated) — Per-condition tool call frequency table. Skipped if no condition has tool call data.
8. **Verdict** (auto-generated) — One paragraph: highest/lowest scorer, cheapest/most expensive, whether all tests passed.
9. **Notes** — Free-text notes passed via `notes=` parameter, or "N/A".
10. **Raw Data** (collapsed at bottom) — Reference Output, Output Schema, Scoring Rubric, Per-Condition Run Records (full JSON), Per-Condition Assessments (full JSON), plus optional Context Pack, Navigation Context, Challenge, and Repo Signals.

All JSON dumps live exclusively in the Raw Data section — never inline in the body.

## Important Notes

- **Aethyme tooling must be up to date**: Always run step 0 before generating artifacts. A stale engine binary or outdated eval/scoring code will produce results that aren't comparable across runs. Record the Aethyme commit hash alongside results.
- **Leverage prompt references a file**: The navigation context file at `/tmp/aethyme-eval-navigation-context.json` must exist before the leverage agent starts. The artifact generation step (step 1) creates it.
- **Leverage prompt env var**: The default prompt says `Use AETHYME_EVAL_NAVIGATION_CONTEXT_FILE` which is an env var contract for runner scripts. For Chau7 execution, the artifact generation rewrites it to `Read the navigation context file at /tmp/aethyme-eval-navigation-context.json`.
- **Scoring applies path normalization** before comparison: markdown links, line anchors, absolute repo prefixes, and leading `./` are stripped. Pass `repo_path` to scoring functions for full normalization.
- **CLI commands need cd prefix**: The graph CLI commands must include `cd <AETHYME_PATH> &&` before `python -m src.cli` because the agent runs from the playground directory, not the Aethyme directory.
- **Token tracking**: Agent token usage is visible in the session output after execution completes. Record it alongside scores for cost analysis.
- **Reports**: Every eval run writes a markdown report under `docs/reports/evals/`.

## Results

### 2026-03-09 — Codex on Aethyme repo (pre-playground, baseline run)

Note: This run was against the Aethyme repo itself (before the playground-only rule). Prompts targeted `Aethyme` not `Aethyme Playground`.

#### Starting Prompts

**Control:**
```
Task: Explain this repo
Repository path: /Users/christophehenner/Downloads/Repositories/Aethyme
Explore the repository directly and produce a structured explanation.
```

**Explore:**
```
Task: Explain this repo
Repository path: /Users/christophehenner/Downloads/Repositories/Aethyme
Explore the repository and produce a structured explanation.

You have access to the following graph navigation commands.
Use them via Bash to explore the repository graph:

  cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && \
    .venv/bin/python -m src.cli repo inspect '<repo>' --json-output
  cd ... && python -m src.cli graph overview '<repo>' --json-output
  cd ... && python -m src.cli graph expand '<repo>' <anchor-id> --json-output

Return only the required structured output.
```

**Leverage:**
```
Task: Explain this repo
Use `AETHYME_EVAL_NAVIGATION_CONTEXT_FILE`.
Return only the required structured output.
```

#### Scores

| | Control | Explore | Leverage |
|---|---------|---------|----------|
| Score | 0.0 / 100 | 40.0 / 100 | 61.2 / 100 |
| Tokens | 101,520 | 57,579 | 49,621 |
| Run Time | not recorded | not recorded | not recorded |

Control scored 0 due to format mismatch (rich markdown vs bare paths), not quality. Qualitatively the most detailed output.

### 2026-03-09 — Codex on Aethyme Playground (first proper playground run)

#### Starting Prompts

**Control:**
```
Task: Explain this repo
Repository path: /Users/christophehenner/Downloads/Repositories/Aethyme Playground
Explore the repository directly and produce a structured explanation.
```

**Explore:**
```
Task: Explain this repo
Repository path: /Users/christophehenner/Downloads/Repositories/Aethyme Playground
Explore the repository and produce a structured explanation.

You have access to the following graph navigation commands.
Use them via Bash to explore the repository graph:

  cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && \
    .venv/bin/python -m src.cli repo inspect '<playground>' --json-output
  cd ... && python -m src.cli graph overview '<playground>' --json-output
  cd ... && python -m src.cli graph expand '<playground>' <anchor-id> --json-output

Return only the required structured output.
```

**Leverage:**
```
Task: Explain this repo
Use `AETHYME_EVAL_NAVIGATION_CONTEXT_FILE`.
Return only the required structured output.
```

#### Scores

| | Control | Explore | Leverage |
|---|---------|---------|----------|
| Score | 0.0 / 100 | 10.67 / 100 | 18.33 / 100 |
| Tokens | 226,169 | 129,187 | 64,055 |
| Run Time | not recorded | not recorded | not recorded |

Per-field breakdown:

| Field | Control | Explore | Leverage |
|-------|---------|---------|----------|
| code_areas | 0% | 0% | 67% |
| reference_areas | 0% | 0% | 0% |
| entrypoints | 0% | 0% | 0% |
| important_docs | 0% | 33% | 0% |
| key_configs | 0% | 50% | 50% |
| key_languages | 0% | 0% | 0% |
| high_risk_areas | 0% | 0% | 0% |
| navigation_order | 0% | 0% | 0% |
| representative_code_files | 0% | 0% | 0% |
| representative_docs | 0% | 33% | 0% |

#### Qualitative Assessment

**Control** (0.0/100, 226K tokens): Produced the richest, most detailed output. 9 code areas with prose descriptions, deep evidence with line-number citations, found doc drift between legacy `src/*` paths and actual `packages/app-shared` structure. Identified 547 tests via direct inspection. All paths wrapped in markdown links with absolute paths + line anchors, causing 0% match on every scored field.

**Explore** (10.67/100, 129K tokens): Used Aethyme graph signals to augment exploration. Reported `entrypoint_clarity=100/strong`, `config_hygiene=21/weak`, `hidden_coupling=23/weak` — metrics neither other condition surfaced. 9 code areas, 14 entrypoints, 15 configs, 16-step navigation order. Found `run-order66.mjs` script. Mix of prose and bare paths gave partial matches on configs (50%), docs (33%), and representative_docs (33%).

**Leverage** (18.33/100, 64K tokens): Most token-efficient (3.5x fewer than Control). Cleanest bare-path format. 12 code areas including `e2e` as a distinct area. Noted React 19 version. Referenced `contracts/README.md` and `docs/agents/context/README.md`. However, `AETHYME_EVAL_NAVIGATION_CONTEXT_FILE` env var was not set in the Codex sandbox, so the agent fell back to direct exploration — a known prompt issue for Chau7 execution (see Important Notes). Bare paths gave best match rate: code_areas 67%, key_configs 50%.

#### Comparison

| Dimension | Control | Explore | Leverage |
|-----------|---------|---------|----------|
| Depth | Deepest — read source code, cited line numbers, found doc drift | Deep — used graph signals + file inspection | Broadest coverage but shallower per-item |
| Accuracy | Very high, but verbose with absolute paths | High, augmented with graph metrics | High, clean bare paths |
| Structure | Rich prose descriptions per area | Mix of prose + bare paths | Cleanest bare-path format |
| Unique Finds | Doc drift, 547 test count, pre-auth boot pattern details | Graph weakness signals (config_hygiene, hidden_coupling), run-order66.mjs | React 19, e2e area, contracts/README.md |
| Token Efficiency | 226K (3.5x worst) | 129K (2x middle) | 64K (best) |
| Why Score is Low | All paths in markdown links with absolute paths + line anchors | Most paths match but several fields empty | Best format match, but some referenced docs don't exist in reference |

**Verdict**: The gradient (Leverage > Explore > Control) is consistent across runs. Token efficiency is the strongest quantitative signal — Leverage achieves more with dramatically fewer tokens. The exact-match scorer needs path normalization to capture the true quality gradient; qualitatively, all three outputs are strong analyses of the Playground repo.

### 2026-03-09 — navigation-ctf: Codex on Aethyme Playground

Reference answer: `packages/auth/package.json` → `packages/ui/src/tokens/index.ts` → area `packages`

#### Starting Prompts

**Control:**
```
Task: Find the manifest that manages the main code entrypoint in the packages area,
identify the entrypoint file it controls, and name the top-level area that owns both.
Repository path: /Users/christophehenner/Downloads/Repositories/Aethyme Playground
Explore the repository directly and produce a structured explanation.
```

**Explore:**
```
Task: Find the manifest that manages the main code entrypoint in the packages area,
identify the entrypoint file it controls, and name the top-level area that owns both.
Repository path: /Users/christophehenner/Downloads/Repositories/Aethyme Playground
Explore the repository and produce a structured explanation.

You have access to the following graph navigation commands.
Use them via Bash to explore the repository graph:

  cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && \
    .venv/bin/python -m src.cli task anchors --repo '<playground>' --task <task> --json-output
  cd ... && python -m src.cli task scope --repo '<playground>' --task <task> --json-output
  cd ... && python -m src.cli graph configs '<playground>' packages --json-output
  cd ... && python -m src.cli graph expand '<playground>' <anchor-id> --json-output

Return only the required structured output.
```

**Leverage:**
```
Task: Find the manifest that manages the main code entrypoint in the packages area,
identify the entrypoint file it controls, and name the top-level area that owns both.
Read the navigation context file at /tmp/aethyme-eval-ctf-navigation-context.json
for pre-computed anchors, scope, and CLI commands.
Use it as your primary navigation layer. Return only the required structured output.
```

#### Scores

| | Control | Explore | Leverage |
|---|---------|---------|----------|
| Score | 0.0 / 100 | 20.0 / 100 | 80.13 / 100 |
| Tokens | 48,026 | 33,730 | 32,864 |
| Run Time | not recorded | not recorded | not recorded |

Per-field breakdown:

| Field (weight) | Control | Explore | Leverage |
|---|---|---|---|
| config_target (30) | 0% — `app-shared/package.json` (absolute path) | 0% — `ui/package.json` (absolute path) | 100% — `auth/package.json` |
| code_target (30) | 0% — `app-shared/src/index.ts` (absolute path) | 0% — `ui/src/index.ts` (absolute path) | 100% — `ui/src/tokens/index.ts` |
| management_area (20) | 0% — `packages/app-shared` | 100% — `packages` | 100% — `packages` |
| relationship_chain (20) | 0% — prose relations | 0% — no chain match | 0.7% — dumped all edges instead of specific chain |

#### Qualitative Assessment

**Control** (0.0/100, 48K tokens): Reasoned well — identified `app-shared/package.json` as the main shared package manifest and `app-shared/src/index.ts` as the entrypoint. This is a defensible human answer but doesn't match the graph's reference (which follows config edges from `auth/package.json`). Named `packages/app-shared` as the area instead of top-level `packages`. Used absolute paths, causing format mismatch. Provided thoughtful rejected candidates (ui, auth, types packages). Self-assessed confidence: "medium-high".

**Explore** (20.0/100, 33.7K tokens): Used all 4 graph CLI commands extensively — `task anchors`, `task scope`, `graph configs`, and `graph expand` on multiple nodes. Correctly identified `packages` as the management area (only match). Chose `ui/package.json` as the config and `ui/src/index.ts` as the entrypoint — reasonable from package exploration but missed the graph signal pointing to `auth/package.json`. Used absolute paths for config/code targets. Self-assessed confidence: "medium".

**Leverage** (80.13/100, 32.9K tokens): Got all 3 targets exactly right — `packages/auth/package.json`, `packages/ui/src/tokens/index.ts`, `packages`. Used clean bare relative paths. The only gap is the relationship chain: instead of the 2 specific reference edges, it dumped the entire list of ~150 `entrypoint_for` edges from `auth/package.json`, scoring nearly 0 on that field. Self-assessed confidence: "high".

#### Comparison

| Dimension | Control | Explore | Leverage |
|---|---|---|---|
| Target accuracy | 0/3 (all wrong targets, absolute paths) | 1/3 (area correct) | 3/3 (all correct) |
| Reasoning quality | Best — reasoned from package semantics | Good — used graph tools but picked wrong config | Weakest — parroted graph output |
| Path format | Absolute (always fails exact-match) | Absolute (fails) | Relative bare paths (matches) |
| Relationship chain | Prose descriptions (creative but wrong format) | Correct format, wrong content | Correct format, massively over-included |
| Token efficiency | 48K (worst) | 33.7K (middle) | 32.9K (best) |
| Self-awareness | "medium-high" — appropriately uncertain | "medium" — correctly uncertain | "high" — correctly confident |

**Verdict**: The gradient is dramatic: Leverage (80) >> Explore (20) >> Control (0). Unlike explain-repo where all 3 were qualitatively strong, here Leverage found the exact right answer because the navigation context file contains the graph edges that define the reference. The CTF specifically tests whether the agent can follow graph relations, and only Leverage had those relations pre-computed. Note: the reference answer (`auth/package.json` → `ui/src/tokens/index.ts`) is a graph artifact — a human would likely agree with Control's answer (`app-shared/package.json` → `app-shared/src/index.ts`). This highlights the CTF measures graph navigation fidelity, not general understanding.

#### Control Rerun — CTO Disabled (2026-03-09)

Rerun of the control condition with Chau7 Command Token Optimization (CTO) forced off to measure whether CTO compression was affecting agent behavior.

**Setup**: Same control prompt as above. Chau7 tab with `tab_set_cto(override="forceOff")`. Codex v0.112.0, gpt-5.4, `approval: never`, `sandbox: workspace-write`, `reasoning effort: high`.

**Notable behavior**: The agent discovered a Codex `aethyme-navigation` skill and attempted to use the Aethyme CLI from the Playground directory (`python3 -m src.cli repo inspect ...`), which failed (`No module named src.cli`). It then fell back to direct file exploration — `find`, `sed`, `rg` across `packages/*/package.json`.

| | Control (CTO on, original) | Control (CTO off, rerun) |
|---|---|---|
| Score | 0.0 / 100 | 20.0 / 100 |
| Tokens | 48,026 | 96,366 |

Per-field breakdown (rerun):

| Field (weight) | Result |
|---|---|
| config_target (30) | 0% — `packages/ui/package.json` (absolute path) |
| code_target (30) | 0% — `packages/ui/src/index.ts` (absolute path) |
| management_area (20) | 100% — `packages` |
| relationship_chain (20) | 0% — prose markdown relations with line anchors |

**Result**: `ui/package.json` → `ui/src/index.ts` → area `packages`. Config and code targets differ from both the original control run (`app-shared`) and the reference (`auth`). Management area correct. Absolute paths and markdown link formatting in all fields. Confidence: "high". Rejected candidates: `app-shared/package.json` (wildcard exports, no root `.` binding).

**Analysis**: CTO off doubled token usage (48K → 96K) but improved the score (0 → 20) by correctly identifying the top-level `packages` area. The agent chose `ui/package.json` — a different wrong answer than the original control's `app-shared/package.json`. Both are defensible human answers; neither matches the graph reference (`auth/package.json`). The Codex aethyme-navigation skill activation is a confound — the agent tried graph CLI commands that weren't part of the control prompt, suggesting skill contamination. CTO doesn't appear to be the primary factor in control's low scores; path format (absolute vs relative) and the fundamental mismatch between "reasonable human answer" and "graph-derived reference" remain the dominant issues.

---

## Playground Setup

See [playground-setup.md](playground-setup.md) for the full setup guide.

**Scripts:**
- `scripts/eval/setup-playground.sh` — clone, sanitize, deploy, verify
- `scripts/eval/verify-playground.sh` — validate an existing playground
- `scripts/eval/run-eval.sh` — end-to-end eval run (verify → server → launch → wait → results)

---

## Eval Type Registry

| Type | Target | Task | Scoring |
|---|---|---|---|
| `bug-fix` | grc | Fix failing test (implication-share) | test pass + regression + code quality + efficiency |
| `bug-fix-1` | mediawiki | Diagnose T419918 watchlist bug | files identified + root cause + fix plan + efficiency |
| `explain-repo` | any | Explain repository architecture | structural accuracy vs engine reference |
| `navigation-ctf` | any | Find manifest → entrypoint → area chain | exact path matching |
| `impact-analysis` | mediawiki | List all callers of doViewUpdates() | call site recall + precision |
| `feature-localization` | mediawiki | Trace Watch button execution chain | ordered method chain matching |
| `config-audit` | mediawiki | Find rate limiting config + enforcement | exact variable/file/class matching |
| `dead-code` | mediawiki | Find unused public functions in Watchlist/ | function recall + precision + efficiency |
| `migration` | mediawiki | List files referencing WatchedItemStore | file recall + precision + efficiency |

Schemas and references: `src/eval/schemas.py`
Scoring functions: `src/eval/scoring.py`
Orchestrator registration: `src/eval/orchestrator.py` (`_EVAL_TYPE_DEFAULTS`)
Server task text: `packages/aethyme-eval-ui/server/main.py` (`EVAL_TASKS`)

---

## Known Pitfalls (Learned 2026-04-13)

### Git History Leaks

`git log --all` traverses ALL refs including remote tracking branches. If the source repo has a fix commit on `origin/master`, any agent that searches git history finds the answer for free — even if the checkout is at a pre-fix commit.

**Fix:** Remove remotes, delete local branches, prune unreachable objects. The `setup-playground.sh` script does this automatically. Always verify with `git log --all --oneline | grep "<fix-description>"`.

### Control Repo Contamination

Chau7 creates `.chau7/snippets/` when opening a tab in a directory. Claude Code may create `.claude/` config dirs. These should not be present in the Control repo.

**Fix:** Run `verify-playground.sh` before every eval. Delete `.chau7/` and `.claude/` from Control if found. Never run engine commands against Control.

### Output Capture

The agent's final text response is difficult to capture programmatically:

| Source | Issue |
|---|---|
| Session JSONL | Final assistant response not written to file |
| `tab_output` (buffer) | Scrollback buffer too small for agents with many tool calls |
| `tab_output` (pty_log) | TUI spinner frames split keywords across lines |
| File-based output | Best path when the agent obeys the prompt; missing file means fall back to PTY |

**Current approach:** instruct each condition to write `.aethyme-eval-output-<condition>.md`, use that file if present, and fall back to PTY log with `_clean_pty_output()` + whitespace-collapsed keyword matching only when the file is missing.

**Pending Chau7 fix:** `tab_last_response` API that returns the last agent text block at the application level, bypassing terminal rendering.

### Score Inflation from Prompt Keywords

The leverage prompt includes navigation context (function listings, file structure). If reference keywords appear in the prompt, the scorer matches them against the prompt text, not the agent's analysis.

**Fix:** The `_score_output()` function strips prompt text before keyword matching. Always pass the condition's prompt to the scorer.

### CTO Overhead on Large Repos

On repos with 12K+ files, CTO (Context Tree Optimization) can increase token usage because the file tree is injected into every context window turn. On MediaWiki, control-cto-on sometimes costs MORE than cto-off.

**Implication:** CTO's value is repo-size-dependent. For large repos, the navigation context from Aethyme tools is more cost-effective than the full file tree from CTO.

---

## Scoring Architecture

### Server-Side Scoring (`_score_output`)

Located in `packages/aethyme-eval-ui/server/main.py`. Runs after output collection, before DB insert.

1. **Prompt stripping**: removes significant words (>5 chars) from the prompt to prevent inflation
2. **Whitespace collapsing**: `re.sub(r'\s+', ' ', output)` to handle TUI noise splitting keywords
3. **Keyword matching**: case-insensitive substring search against reference keywords
4. **Efficiency scoring**: `reference_cost / (reference_cost + actual_cost)` — smooth curve rewarding lower cost

### Formal Scoring (`src/eval/scoring.py`)

Used for structured output comparison. Requires the agent's output as a parsed dict (not available from PTY log). Includes:

- `_normalize_path()` — strips markdown links, line anchors, absolute prefixes
- `_keyword_score()` — fraction of reference keywords present
- `_efficiency_score()` — cost-relative scoring
- `_compute_guardrails()` — detects formatting issues (absolute paths, markdown links)

### Scoring Limitations

- **PTY log scoring** is keyword-based and approximate. An agent could mention "doViewUpdates" in a wrong context and still get credit.
- **Formal scoring** requires structured JSON output, which the current output capture doesn't provide.
- **Cross-condition comparison** is only reliable for efficiency metrics (tokens, cost, tools, duration). Quality scores should be interpreted with caution until output capture is solved.
