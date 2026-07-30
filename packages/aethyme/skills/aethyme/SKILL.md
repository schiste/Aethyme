---
name: aethyme
description: Use Aethyme's high-level Explore intents, current repository
  analyzers, and code graph for navigation, caller tracing, derived facts,
  dead-code analysis, and task context.
---

# Aethyme Navigation

Use this skill for repository navigation, task localization, caller tracing,
dead-code analysis, graph context, or compact task packs. Start with this
contract; load a reference only if the first result is insufficient.

## Setup

```bash
AETHYME_ROOT="{{AETHYME_ROOT}}"
AETHYME_BIN="$AETHYME_ROOT/rust/target/release/aethyme"
AETHYME_PY="$AETHYME_ROOT/.venv/bin/python"
REPO="$PWD"
```

Important: `python -m src.cli ...` commands are retired. Use the `aethyme`
binary for graph, task, facts, intents, analyze, enhance, and Explore.

## Default Contract

1. Make one bounded Explore call before broad manual search. Save the full JSON
   to a temp file and print only the compact projection:

```bash
AETHYME_JSON="$(mktemp -t aethyme-explore.XXXXXX.json)"
"$AETHYME_BIN" explore --repo "$REPO" --request "<user request>" --format answer-json --show-observability --depth 0 > "$AETHYME_JSON"
"$AETHYME_PY" - "$AETHYME_JSON" <<'PY'
import json, sys; d = json.load(open(sys.argv[1], encoding="utf-8"))
targets = []; lanes = d.get("subsystems", [])[:3]
subsystems = [{k: lane.get(k) for k in ("rank", "id", "label", "role", "confidence", "token_subsystems", "missing_coverage_warnings")} for lane in lanes]
for lane in lanes:
    for target in lane.get("top_verification_targets", [])[:2]:
        if isinstance(target, dict):
            row = dict(target); row.setdefault("subsystem", lane.get("role") or lane.get("id"))
            targets.append(row)
print(json.dumps({
    "safe_to_use_as_answer": d.get("safe_to_use_as_answer"),
    "trust_policy": d.get("trust_policy"),
    "subsystems": subsystems,
    "top_verification_targets": targets[:6],
    "verification_steps": d.get("verification_steps", [])[:3],
    "observability": {"readiness": d.get("observability", {}).get("readiness")},
}, indent=2))
PY
```

2. Inspect only: `safe_to_use_as_answer`, `trust_policy`, `subsystems`,
   `top_verification_targets`, `verification_steps`, and
   `observability.readiness`.

3. Verify with bounded source spans before manual reads:

```bash
"$AETHYME_BIN" verify-targets --repo "$REPO" --from "$AETHYME_JSON" --max-targets 2 --max-lines 80
```

4. Use the returned spans first. Read full target files only if those spans are
   insufficient: at most 2-3 files, about 80-120 relevant lines each.

5. If `safe_to_use_as_answer=false`, follow `verification_steps` and the top
   subsystem lanes as an investigation plan. Do not run broad `rg`, `rg
   --files`, or repository-wide grep unless the top targets fail.

6. Escalate deliberately. Prefer one deeper Explore call over several unrelated
   commands. Use `--depth 1/2/3` only when the previous result did not provide
   enough evidence to act.

## Load References Only When Needed

- `references/explore.md`: depth ladder, intent choice, trust/observability,
  and bounded retry rules.
- `references/graph-task.md`: graph views, callers/callees, task scope,
  context-pack, and prompt-pack commands.
- `references/dead-code.md`: usage-boundary, dead-code, public API, facts, and
  ambiguity handling.

## When Not To Use Aethyme

- A simple file read, exact path lookup, or tiny grep already answers the task.
- You already have one decisive Aethyme result and only need narrow source
  verification.
- The task asks for eval baselines, prior reports, or generated reference
  artifacts as evidence; those must not be used for benchmark answers.
