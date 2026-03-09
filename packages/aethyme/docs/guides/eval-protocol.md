# Eval Protocol — Aethyme Navigation Benchmarks

Last Updated: 2026-03-09

## Core Rule

**Evaluations run only against Playground repositories.** Never run evals against the Aethyme repo itself — it conflates the tool with the subject.

## Local Eval Storage

Eval artifacts are stored locally under `packages/aethyme/eval-runs/` (gitignored). Each run creates a timestamped directory:

```
eval-runs/
  YYYYMMDD-HHMMSS-<slug>-<eval-type>/
    metadata.json              # timestamp, aethyme commit, repo path, eval type
    report.md                  # rendered markdown report
    artifacts/                 # inputs and reference data
      control-prompt.txt
      explore-prompt.txt
      leverage-prompt.txt
      output-schema.json
      scoring-rubric.json
      reference-output.json
      navigation-context.json
      pack.json
    conditions/                # per-condition outputs
      control/
        result.json            # structured output from agent
        assessment.json        # scoring assessment
      explore/
        result.json
        assessment.json
      leverage/
        result.json
        assessment.json
    chau7/                     # manually populated telemetry
      control/
        run-metadata.json
        transcript.json
        tool-calls.json
      explore/
        ...
      leverage/
        ...
```

| Artifact | Location | Created by |
|----------|----------|------------|
| Run directory | `eval-runs/<timestamp>-<slug>-<type>/` | `create_eval_run_dir()` |
| Prompts, schemas, reference | `artifacts/` | `write_eval_run_artifacts()` |
| Agent outputs | `conditions/<cond>/result.json` | `write_eval_run_artifacts()` |
| Scoring assessments | `conditions/<cond>/assessment.json` | `write_eval_run_artifacts()` |
| Rendered report | `report.md` | `write_*_markdown_report(run_dir=...)` |
| Chau7 telemetry | `chau7/<cond>/` | Manual capture (Step 4.5) |

## Playground Repositories

| Name | Path | Origin | Notes |
|------|------|--------|-------|
| Aethyme Playground | `/Users/christophehenner/Downloads/Repositories/Aethyme Playground` | `https://github.com/Aeptus/mockup.git` | Enterprise GRC platform (Django + React + infra) |

To add a new playground: clone a real repo, add it to this table, and reference it from the docs index.

## 3-Condition Design

Every eval compares three conditions to isolate the value of graph-derived navigation context:

| Condition | What the agent receives | Nav context file | Graph CLI in prompt | What it tests |
|-----------|------------------------|-----------------|---------------------|---------------|
| **Control** | Task + repo path only | No | No | Raw LLM exploration ability |
| **Explore** | Task + repo path + CLI commands | No | Yes | Whether graph tools alone help |
| **Leverage** | Task + pre-computed navigation context file | Yes | Yes (in context) | Whether pre-computed graph analysis adds value |

## Eval Types

### explain-repo

Task: "Explain this repo" — produces a structured overview with code areas, entrypoints, docs, configs, languages, risks, navigation order, and evidence.

#### Exact Prompts

**Control prompt:**

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

**Control prompt:**

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

| Dimension | Control | Explore | Leverage |
|-----------|---------|---------|----------|
| Target accuracy | ... | ... | ... |
| Reasoning quality | ... | ... | ... |
| Path format | ... | ... | ... |
| Relationship chain | ... | ... | ... |
| Token Efficiency | ... | ... | ... |
| Self-awareness | ... | ... | ... |

## Execution Method

Evals are orchestrated by spinning up AI agent sessions (via terminal multiplexer, Chau7 MCP, or equivalent) and feeding them the generated prompts. This is **not** done through runner scripts.

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

result = run_explain_repo_evaluation(PLAYGROUND)

# Write prompt files (also to /tmp/ for agent access)
Path('/tmp/aethyme-eval-control-prompt.txt').write_text(result['control']['prompt'])
Path('/tmp/aethyme-eval-explore-prompt.txt').write_text(result['explore']['prompt'])
Path('/tmp/aethyme-eval-leverage-prompt.txt').write_text(result['leverage']['prompt'])
Path('/tmp/aethyme-eval-output-schema.json').write_text(json.dumps(result['output_schema'], indent=2))
Path('/tmp/aethyme-eval-navigation-context.json').write_text(json.dumps(result['navigation_context'], indent=2))
Path('/tmp/aethyme-eval-reference.json').write_text(json.dumps(result['reference_output']))
```

This produces:
- `/tmp/aethyme-eval-control-prompt.txt`
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
tab_create(directory="/Users/christophehenner/Downloads/Repositories/Aethyme Playground")  # x3
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

Where `<CONDITION>` is `control`, `explore`, or `leverage`.

Any agent that can accept a system prompt and produce structured JSON output works. The output must conform to the schema at `/tmp/aethyme-eval-output-schema.json`.

#### 4. Monitor

Record the **wall-clock start time** before launching each agent. Wait for each session to finish, then record the end time:
```
# Before launch: note the time
# After completion:
tab_status(tab_id=<id>)    # "idle" means done
tab_output(tab_id=<id>, lines=15)  # token counts at bottom
```

Compute run time as `end - start` per condition. Report in the scores table as `Xm Ys`.

#### 4.5. Capture Chau7 Telemetry

After all agents finish, capture run telemetry for each condition. Get run IDs from `tab_status` or via `session_list` + `run_list`:

```python
# For each condition tab:
status = tab_status(tab_id=<id>)
run_id = status["active_run"]["id"]  # or find via run_list

# Capture telemetry
metadata = run_get(run_id=run_id)
transcript = run_transcript(run_id=run_id)
tool_calls = run_tool_calls(run_id=run_id)

# Save to run directory
import json
from pathlib import Path
cond = "control"  # or "explore", "leverage"
chau7_dir = run_dir / "chau7" / cond
chau7_dir.mkdir(parents=True, exist_ok=True)
(chau7_dir / "run-metadata.json").write_text(json.dumps(metadata, indent=2))
(chau7_dir / "transcript.json").write_text(json.dumps(transcript, indent=2))
(chau7_dir / "tool-calls.json").write_text(json.dumps(tool_calls, indent=2))
```

This step is optional but strongly recommended — telemetry enables the Tool Call Analysis section in the report.

#### 5. Collect and Score

```python
import json
from pathlib import Path
from src.eval.scoring import score_explain_repo_output

reference = json.loads(Path('/tmp/aethyme-eval-reference.json').read_text())
for cond in ("control", "explore", "leverage"):
    candidate = json.loads(Path(f"/tmp/aethyme-eval-{cond}-result.json").read_text())
    assessment = score_explain_repo_output(candidate, reference)
    print(f"{cond}: {assessment['weighted_score']} / {assessment['max_score']}")
```

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

| Dimension | Control | Explore | Leverage |
|-----------|---------|---------|----------|
| Depth | ... | ... | ... |
| Accuracy | ... | ... | ... |
| Structure | ... | ... | ... |
| Unique Finds | ... | ... | ... |
| Token Efficiency | ... | ... | ... |
| Why Score is Low | ... | ... | ... |

## Results Report Template

Every eval results section **must** include the following, in order:

1. **Header** — `### <date> — <eval-type>: <agent> on <playground>` plus reference answer if applicable.
2. **Starting Prompts** — The exact verbatim prompt text for each condition (Control, Explore, Leverage), in fenced code blocks.
3. **Scores** — A table with one row per metric:

| | Control | Explore | Leverage |
|---|---------|---------|----------|
| Score | ... / 100 | ... / 100 | ... / 100 |
| Tokens | ... | ... | ... |
| Run Time | ...m ...s | ...m ...s | ...m ...s |

4. **Per-field breakdown** (if applicable) — A table showing each scored field with its weight and per-condition result.
5. **Qualitative Assessment** — One paragraph per condition summarizing behavior, strengths, failures, and confidence.
6. **Tool Call Analysis** (auto-generated) — Per-condition tool call frequency table and CLI commands list. Skipped if no condition has tool call data.
7. **Context Pack Audit** (auto + manual) — Summary stats (anchor count, nav order items, in-scope files, CLI commands), navigation context JSON dump, and placeholder for manual signal-to-noise assessment.
8. **Comparison** — A table comparing conditions across: Depth, Accuracy, Structure, Unique Finds, Token Efficiency, Why Score is Low.
9. **Verdict** — One paragraph summarizing the gradient and key takeaway.
10. **Graph Quality Notes** (manual) — Post-run analysis of graph structural quality and its impact on conditions.
11. **Prompt Effectiveness** (manual) — Post-run analysis of how well each condition's prompt served the agent.
12. **Lessons & Action Items** (manual) — Checklist of improvements for next run.

Omitting any of these sections makes the report non-compliant. When run time was not recorded, mark it as `not recorded`.

## Important Notes

- **Aethyme tooling must be up to date**: Always run step 0 before generating artifacts. A stale engine binary or outdated eval/scoring code will produce results that aren't comparable across runs. Record the Aethyme commit hash alongside results.
- **Leverage prompt references a file**: The navigation context file at `/tmp/aethyme-eval-navigation-context.json` must exist before the leverage agent starts. The artifact generation step (step 1) creates it.
- **Leverage prompt env var**: The default prompt says `Use AETHYME_EVAL_NAVIGATION_CONTEXT_FILE` which is an env var contract for runner scripts. For Chau7 execution, the artifact generation rewrites it to `Read the navigation context file at /tmp/aethyme-eval-navigation-context.json`.
- **Scoring is exact-match**: Outputs with markdown links (`src/cli.py#L159`) or prose descriptions score 0 against bare-path references (`src/cli.py`). Path normalization is a known improvement needed.
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
