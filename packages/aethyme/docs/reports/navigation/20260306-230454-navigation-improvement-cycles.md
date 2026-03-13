# Navigation Improvement Cycles Report

- Last Updated: 2026-03-06
- Scope: Aethyme Core graph navigation and eval refinement
- Repo under test: `/Users/christophehenner/Downloads/Repositories/ADD`

## Objective

Run three consecutive improvement cycles on the graph-mediated navigation path.
Each cycle had to:
1. identify real issues from current outputs,
2. implement fixes without silencing warnings,
3. lint and test cleanly,
4. inspect the new behavior,
5. derive the next improvements.

## Final Validation

### Python lint

```text
All checks passed!
```

### Python tests

```text
23 passed in 0.71s
```

### Rust tests

```text
15 passed; 0 failed
```

## Cycle 1

### Problems identified
1. The graph existed, but agents still did not have a true graph-mediated interface.
2. Task scope leaked outside the intended subsystem once anchors were resolved.
3. Evals still behaved like one-shot prompt artifacts rather than iterative navigation contracts.

### Changes implemented
- Added graph navigation commands:
  - `graph node`
  - `graph children`
  - `graph parents`
  - `graph callers`
  - `graph callees`
  - `graph docs`
  - `graph configs`
- Added task navigation commands:
  - `task anchors`
  - `task scope`
  - `task next`
  - `task expand`
- Added compact graph slice command:
  - `graph expand`
- Extended the eval runner contract with:
  - `AETHYME_EVAL_NAVIGATION_CONTEXT_FILE`
  - `AETHYME_EVAL_NAVIGATION_CONTEXT`
- Switched eval prompts to iterative-navigation prompts that point to navigation context instead of large inline packs.

### Key files
- `packages/aethyme/rust/crates/aethyme-engine/src/navigation.rs`
- `packages/aethyme/rust/crates/aethyme-engine/src/json.rs`
- `packages/aethyme/rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs`
- `packages/aethyme/src/indexing/engine.py`
- `packages/aethyme/src/cli.py`
- `packages/aethyme/src/eval/runner.py`
- `packages/aethyme/src/eval/control_prompt.py`
- `packages/aethyme/src/eval/explain_repo.py`
- `packages/aethyme/src/eval/navigation_ctf.py`
- `packages/aethyme/src/eval/report.py`

### Result
- Agents can now navigate the repograph directly instead of only receiving one-shot packs.
- `graph expand GameEngine/rust/addgame/Cargo.toml` returns a compact config-centric slice.
- `ExplainRepo` and `NavigationCTF` evals now support iterative navigation context.

## Cycle 2

### Problems identified after Cycle 1
1. Task anchors still included off-area files and symbols even when the task explicitly named `GameEngine`.
2. Navigation order inherited those anchors, so the top of the navigation path still drifted.

### Changes implemented
- Moved primary-area filtering into anchor resolution itself.
- When a folder/area anchor exists, only anchors inside that area survive for non-`ExplainRepo` tasks.
- Scope and navigation now inherit cleaner anchors automatically.

### Key files
- `packages/aethyme/rust/crates/aethyme-engine/src/anchors.rs`

### Result on ADD

Before:
- anchors included `godot/project.godot`
- anchors included `godot/tools/osm_to_hex.py::main`

After:
```json
{
  "anchors": [
    {"kind": "folder", "id": "GameEngine"},
    {"kind": "file", "id": "GameEngine/godot/project.godot"},
    {"kind": "file", "id": "GameEngine/rust/addgame/Cargo.toml"}
  ]
}
```

Navigation order became:
```json
[
  "GameEngine",
  "GameEngine/godot/project.godot",
  "GameEngine/rust/addgame/Cargo.toml"
]
```

## Cycle 3

### Problems identified after Cycle 2
1. Non-`ExplainRepo` task packs still carried too much dependency/impact noise.
2. Node views could repeat low-value annotations.
3. Scope was improved, but task packs still exposed more cross-area noise than they should.

### Changes implemented
- Deduplicated graph annotations in node views.
- Rebuilt non-`ExplainRepo` dependency/impact derivation to be:
  - edge-type-aware,
  - area-bounded,
  - truncated,
  - lower-noise.
- Added a Rust regression test proving config-navigation packs stay inside the matched area.

### Key files
- `packages/aethyme/rust/crates/aethyme-engine/src/navigation.rs`
- `packages/aethyme/rust/crates/aethyme-engine/src/pipeline.rs`

### Result on ADD

Final `task scope` for:
`Find the manifest that manages the main code entrypoint in the GameEngine area, identify the entrypoint file it controls, and name the top-level area that owns both.`

```json
{
  "navigation_order": [
    "GameEngine",
    "GameEngine/godot/project.godot",
    "GameEngine/rust/addgame/Cargo.toml"
  ],
  "in_scope_files": [
    "GameEngine/godot/project.godot",
    "GameEngine/rust/addgame/Cargo.toml"
  ],
  "in_scope_symbols": [],
  "in_scope_areas": ["GameEngine"],
  "out_of_scope": [
    ".chau7",
    ".claude",
    "content",
    "documentation",
    "godot",
    "lore",
    "mechanics",
    "tools"
  ]
}
```

Final `task pack` now has only one direct dependency and three bounded impact items for this task instead of a broad mixed frontier.

## Final Eval State

### ExplainRepo
Latest report:
- `packages/aethyme/docs/reports/evals/20260306-215846-add-explain-repo.md`

Current metrics:
- baseline prompt chars: `161`
- Aethyme prompt chars: `111`

### NavigationCTF
Latest report:
- `packages/aethyme/docs/reports/evals/20260306-215931-add-navigation-ctf.md`

Current metrics:
- baseline prompt chars: `309`
- Aethyme prompt chars: `259`

### Important note
No external runner command was supplied during these evals, so the reports still contain:
- `baseline_run: null`
- `aethyme_run: null`
- no live structured scoring yet

The eval framework is ready for iterative graph navigation, but a real agent/model command is still needed to exercise it end to end.

## Verbose Eval Results

### ExplainRepo verbose result

#### Reference / Aethyme structured output

```json
{
  "repo_summary": "Task: Explain this repo",
  "main_areas": [
    "GameEngine",
    "tools"
  ],
  "entrypoints": [
    "godot/tools/osm_to_hex.py"
  ],
  "important_docs": [
    "README.md",
    "content/README.md",
    "documentation/technical-architecture.md"
  ],
  "key_configs": [
    "GameEngine/godot/project.godot",
    "GameEngine/rust/addgame/Cargo.toml"
  ],
  "key_languages": [
    "python",
    "rust"
  ],
  "high_risk_areas": [],
  "navigation_order": [
    "README.md",
    "documentation/technical-architecture.md",
    "GameEngine",
    "tools",
    "godot/tools/osm_to_hex.py"
  ],
  "evidence": [
    "README.md",
    "documentation/technical-architecture.md",
    "godot/tools/osm_to_hex.py"
  ]
}
```

#### Runner status

```json
{
  "baseline_assessment": null,
  "aethyme_assessment": null,
  "baseline_run": null,
  "aethyme_run": null
}
```

### NavigationCTF verbose result

#### Reference / Aethyme structured output

```json
{
  "config_target": {
    "path": "GameEngine/rust/addgame/Cargo.toml",
    "why": "manifest/config linked to the runtime entrypoint"
  },
  "code_target": {
    "path": "GameEngine/rust/addgame/src/lib.rs",
    "why": "entrypoint file linked by the configuration graph"
  },
  "management_area": {
    "name": "GameEngine",
    "why": "top-level area linked by the configuration graph"
  },
  "relationship_chain": [
    {
      "from": "GameEngine/rust/addgame/Cargo.toml",
      "to": "GameEngine",
      "relation": "configures"
    },
    {
      "from": "GameEngine/rust/addgame/Cargo.toml",
      "to": "GameEngine/rust/addgame/src/lib.rs",
      "relation": "entrypoint_for"
    }
  ],
  "rejected_candidates": [],
  "confidence": "high"
}
```

#### Runner status

```json
{
  "baseline_assessment": null,
  "aethyme_assessment": null,
  "baseline_run": null,
  "aethyme_run": null
}
```

## What improved materially
1. The graph is now directly navigable by command surface.
2. Task navigation is area-aware instead of only symbol-name-driven.
3. Iterative navigation context is now a first-class eval input.
4. The Aethyme eval prompt is now smaller than baseline on both current benchmark classes.
5. Config-management tasks on `ADD` now land on the right area and manifest slice.

## Remaining issues
1. `GameEngine/godot/project.godot` still appears alongside `GameEngine/rust/addgame/Cargo.toml` for the config-management task. It is valid but not ideal.
2. `task kind` is still `unknown` for this navigation task family, so the pipeline relies on heuristic anchor rules rather than a dedicated task class.
3. The evals are structurally ready, but there is still no real model runner attached.

## Recommended next moves
1. Introduce explicit navigation task kinds instead of leaving manifest/ownership tasks as `unknown`.
2. Rank configs by stronger ownership and entrypoint evidence so Cargo/project files separate more cleanly.
3. Attach a real evaluation runner and score live structured outputs against the new iterative-navigation contract.
