# Eval Report: Find all public top-level functions in `packages/aethyme/src/indexing/` that are never called from outside that directory.

Scope:
- Check every Python file in `packages/aethyme/src/indexing/` for public top-level function definitions
- For each public function, search the entire repo outside `packages/aethyme/src/indexing/` for call sites
- Search at least `packages/aethyme/src/`, `packages/aethyme/tests/`, and `packages/aethyme/scripts/`
- Exclude private helpers whose names start with `_`

For each unused function, report:
- The function name
- The file it's defined in (relative path)
- Why you believe it's unused (what you searched for and didn't find)

Be thorough — check every public top-level function, not just a sample. Missing a truly unused function or falsely flagging a used one both count against you.

## Meta

- Date: 2026-04-20
- Repository: `aethyme`
- Eval Type: dead-code
- Conditions: 
- Aethyme Commit: `e5b2a970071a7ea9e7d66c52d86ece6876dd7cb1`

## Model

- Name: haiku
- Provider: anthropic
- Backend: claude
- Reasoning: default
- Permission Mode: N/A

## Scorecard

| Condition | Quality | Recalculated Eval | Tools | Cost | Duration | Total Tokens | Score / 1K Tokens | Score / Minute |
|---|---|---|---|---|---|---|---|---|

## Prompts

## Agent Output

## Verdict

N/A

## Notes

N/A

---

## Raw Data

### Reference Output

```json
{
  "baseline_id": "aethyme-dead-code-indexing-v1",
  "reviewed_at": "2026-04-17",
  "selection_rule": "Public top-level functions in packages/aethyme/src/indexing/ with zero callers outside that directory across packages/aethyme/src, packages/aethyme/tests, and packages/aethyme/scripts.",
  "unused_functions": [
    {
      "function_name": "setup_indexing_logging",
      "defined_in": "packages/aethyme/src/indexing/logging.py",
      "reason": "No callers were found outside packages/aethyme/src/indexing/."
    },
    {
      "function_name": "create_indexing_logger",
      "defined_in": "packages/aethyme/src/indexing/logging.py",
      "reason": "No callers were found outside packages/aethyme/src/indexing/."
    },
    {
      "function_name": "get_metrics_text",
      "defined_in": "packages/aethyme/src/indexing/metrics.py",
      "reason": "No callers were found outside packages/aethyme/src/indexing/."
    },
    {
      "function_name": "ensure_default_scope",
      "defined_in": "packages/aethyme/src/indexing/service.py",
      "reason": "No callers were found outside packages/aethyme/src/indexing/."
    },
    {
      "function_name": "resolve_scope",
      "defined_in": "packages/aethyme/src/indexing/service.py",
      "reason": "No callers were found outside packages/aethyme/src/indexing/."
    },
    {
      "function_name": "index_repository",
      "defined_in": "packages/aethyme/src/indexing/service.py",
      "reason": "No callers were found outside packages/aethyme/src/indexing/."
    },
    {
      "function_name": "iter_repository_files",
      "defined_in": "packages/aethyme/src/indexing/repository_snapshot.py",
      "reason": "No callers were found outside packages/aethyme/src/indexing/."
    },
    {
      "function_name": "build_engine_run_metadata",
      "defined_in": "packages/aethyme/src/indexing/engine.py",
      "reason": "No callers were found outside packages/aethyme/src/indexing/."
    },
    {
      "function_name": "activate",
      "defined_in": "packages/aethyme/src/indexing/engine.py",
      "reason": "No callers were found outside packages/aethyme/src/indexing/."
    },
    {
      "function_name": "activate_from",
      "defined_in": "packages/aethyme/src/indexing/engine.py",
      "reason": "No callers were found outside packages/aethyme/src/indexing/."
    },
    {
      "function_name": "explain_task",
      "defined_in": "packages/aethyme/src/indexing/engine.py",
      "reason": "No callers were found outside packages/aethyme/src/indexing/."
    },
    {
      "function_name": "workspace_inspect",
      "defined_in": "packages/aethyme/src/indexing/engine.py",
      "reason": "No callers were found outside packages/aethyme/src/indexing/."
    },
    {
      "function_name": "workspace_blast_radius",
      "defined_in": "packages/aethyme/src/indexing/engine.py",
      "reason": "No callers were found outside packages/aethyme/src/indexing/."
    }
  ],
  "literal_external_only": [
    {
      "function_name": "setup_indexing_logging",
      "defined_in": "packages/aethyme/src/indexing/logging.py",
      "reason": "No callers were found outside packages/aethyme/src/indexing/."
    },
    {
      "function_name": "create_indexing_logger",
      "defined_in": "packages/aethyme/src/indexing/logging.py",
      "reason": "No callers were found outside packages/aethyme/src/indexing/."
    },
    {
      "function_name": "get_metrics_text",
      "defined_in": "packages/aethyme/src/indexing/metrics.py",
      "reason": "No callers were found outside packages/aethyme/src/indexing/."
    },
    {
      "function_name": "ensure_default_scope",
      "defined_in": "packages/aethyme/src/indexing/service.py",
      "reason": "No callers were found outside packages/aethyme/src/indexing/."
    },
    {
      "function_name": "resolve_scope",
      "defined_in": "packages/aethyme/src/indexing/service.py",
      "reason": "No callers were found outside packages/aethyme/src/indexing/."
    },
    {
      "function_name": "index_repository",
      "defined_in": "packages/aethyme/src/indexing/service.py",
      "reason": "No callers were found outside packages/aethyme/src/indexing/."
    },
    {
      "function_name": "iter_repository_files",
      "defined_in": "packages/aethyme/src/indexing/repository_snapshot.py",
      "reason": "No callers were found outside packages/aethyme/src/indexing/."
    },
    {
      "function_name": "build_engine_run_metadata",
      "defined_in": "packages/aethyme/src/indexing/engine.py",
      "reason": "No callers were found outside packages/aethyme/src/indexing/."
    },
    {
      "function_name": "activate",
      "defined_in": "packages/aethyme/src/indexing/engine.py",
      "reason": "No callers were found outside packages/aethyme/src/indexing/."
    },
    {
      "function_name": "activate_from",
      "defined_in": "packages/aethyme/src/indexing/engine.py",
      "reason": "No callers were found outside packages/aethyme/src/indexing/."
    },
    {
      "function_name": "explain_task",
      "defined_in": "packages/aethyme/src/indexing/engine.py",
      "reason": "No callers were found outside packages/aethyme/src/indexing/."
    },
    {
      "function_name": "workspace_inspect",
      "defined_in": "packages/aethyme/src/indexing/engine.py",
      "reason": "No callers were found outside packages/aethyme/src/indexing/."
    },
    {
      "function_name": "workspace_blast_radius",
      "defined_in": "packages/aethyme/src/indexing/engine.py",
      "reason": "No callers were found outside packages/aethyme/src/indexing/."
    }
  ],
  "likely_dead_code": [
    {
      "function_name": "setup_indexing_logging",
      "defined_in": "packages/aethyme/src/indexing/logging.py",
      "reason": "Public helper with no callers outside indexing and no obvious CLI/API wiring."
    },
    {
      "function_name": "create_indexing_logger",
      "defined_in": "packages/aethyme/src/indexing/logging.py",
      "reason": "Factory wrapper with no callers outside indexing."
    },
    {
      "function_name": "get_metrics_text",
      "defined_in": "packages/aethyme/src/indexing/metrics.py",
      "reason": "Metrics export helper with no callers outside indexing."
    },
    {
      "function_name": "activate",
      "defined_in": "packages/aethyme/src/indexing/engine.py",
      "reason": "Task-activation wrapper is not consumed outside indexing."
    },
    {
      "function_name": "activate_from",
      "defined_in": "packages/aethyme/src/indexing/engine.py",
      "reason": "Seeded activation wrapper is not consumed outside indexing."
    },
    {
      "function_name": "explain_task",
      "defined_in": "packages/aethyme/src/indexing/engine.py",
      "reason": "Text explanation helper has no non-indexing callers."
    },
    {
      "function_name": "workspace_inspect",
      "defined_in": "packages/aethyme/src/indexing/engine.py",
      "reason": "Workspace wrapper has no non-indexing callers."
    },
    {
      "function_name": "workspace_blast_radius",
      "defined_in": "packages/aethyme/src/indexing/engine.py",
      "reason": "Workspace blast-radius wrapper has no non-indexing callers."
    }
  ],
  "scope": {
    "directory": "packages/aethyme/src/indexing/",
    "language": "python",
    "symbol_kind": "top_level_function"
  },
  "exclusions": [
    "Private helpers whose names start with '_'",
    "References inside packages/aethyme/src/indexing/",
    "Non-Python files, build output, .venv, eval-runs, and generated artifacts"
  ],
  "function_keywords": [
    "setup_indexing_logging",
    "create_indexing_logger",
    "get_metrics_text",
    "ensure_default_scope",
    "resolve_scope",
    "index_repository",
    "iter_repository_files",
    "build_engine_run_metadata",
    "activate",
    "activate_from",
    "explain_task",
    "workspace_inspect",
    "workspace_blast_radius"
  ],
  "engineering_review": {
    "likely_dead_code": [
      {
        "function_name": "setup_indexing_logging",
        "defined_in": "packages/aethyme/src/indexing/logging.py",
        "reason": "Public helper with no callers outside indexing and no obvious CLI/API wiring."
      },
      {
        "function_name": "create_indexing_logger",
        "defined_in": "packages/aethyme/src/indexing/logging.py",
        "reason": "Factory wrapper with no callers outside indexing."
      },
      {
        "function_name": "get_metrics_text",
        "defined_in": "packages/aethyme/src/indexing/metrics.py",
        "reason": "Metrics export helper with no callers outside indexing."
      },
      {
        "function_name": "activate",
        "defined_in": "packages/aethyme/src/indexing/engine.py",
        "reason": "Task-activation wrapper is not consumed outside indexing."
      },
      {
        "function_name": "activate_from",
        "defined_in": "packages/aethyme/src/indexing/engine.py",
        "reason": "Seeded activation wrapper is not consumed outside indexing."
      },
      {
        "function_name": "explain_task",
        "defined_in": "packages/aethyme/src/indexing/engine.py",
        "reason": "Text explanation helper has no non-indexing callers."
      },
      {
        "function_name": "workspace_inspect",
        "defined_in": "packages/aethyme/src/indexing/engine.py",
        "reason": "Workspace wrapper has no non-indexing callers."
      },
      {
        "function_name": "workspace_blast_radius",
        "defined_in": "packages/aethyme/src/indexing/engine.py",
        "reason": "Workspace blast-radius wrapper has no non-indexing callers."
      }
    ],
    "internal_public_api": [
      {
        "function_name": "ensure_default_scope",
        "defined_in": "packages/aethyme/src/indexing/service.py",
        "reason": "Public for module organization, but only used internally inside indexing."
      },
      {
        "function_name": "resolve_scope",
        "defined_in": "packages/aethyme/src/indexing/service.py",
        "reason": "Public for service-layer readability, but only used internally inside indexing."
      },
      {
        "function_name": "index_repository",
        "defined_in": "packages/aethyme/src/indexing/service.py",
        "reason": "Secondary entrypoint that may be kept for future use even though no external callers exist."
      },
      {
        "function_name": "iter_repository_files",
        "defined_in": "packages/aethyme/src/indexing/repository_snapshot.py",
        "reason": "Public file iterator with no current callers outside indexing."
      },
      {
        "function_name": "build_engine_run_metadata",
        "defined_in": "packages/aethyme/src/indexing/engine.py",
        "reason": "Compatibility helper mirrored by newer contract code."
      }
    ]
  }
}
```

### Output Schema

```json
{
  "type": "object",
  "additionalProperties": false,
  "required": [
    "unused_functions"
  ],
  "properties": {
    "unused_functions": {
      "type": "array",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": [
          "function_name",
          "defined_in",
          "reason"
        ],
        "properties": {
          "function_name": {
            "type": "string"
          },
          "defined_in": {
            "type": "string"
          },
          "reason": {
            "type": "string"
          }
        }
      }
    }
  }
}
```

### Scoring Rubric

```json
{
  "weights": {
    "functions_found": 60,
    "false_positives": 20,
    "efficiency": 20
  },
  "notes": [
    "functions_found: recall \u2014 how many reviewed Aethyme indexing baseline functions were identified.",
    "false_positives: precision \u2014 penalty for listing functions outside the reviewed indexing baseline.",
    "efficiency: cost relative to $1.00 baseline.",
    "Important: this benchmark uses the literal prompt semantics ('zero non-test callers outside packages/aethyme/src/indexing/'), not a broader maintainability definition of dead code."
  ]
}
```

### Per-Condition Run Records

### Per-Condition Assessments

### Context Pack

```json
{
  "status": "not_generated_in_eval_ui_server",
  "eval_type": "dead-code",
  "task": "Find all public top-level functions in `packages/aethyme/src/indexing/` that are never called from outside that directory.\n\nScope:\n- Check every Python file in `packages/aethyme/src/indexing/` for public top-level function definitions\n- For each public function, search the entire repo outside `packages/aethyme/src/indexing/` for call sites\n- Search at least `packages/aethyme/src/`, `packages/aethyme/tests/`, and `packages/aethyme/scripts/`\n- Exclude private helpers whose names start with `_`\n\nFor each unused function, report:\n- The function name\n- The file it's defined in (relative path)\n- Why you believe it's unused (what you searched for and didn't find)\n\nBe thorough \u2014 check every public top-level function, not just a sample. Missing a truly unused function or falsely flagging a used one both count against you."
}
```

### Navigation Context

```json
{
  "mode": "engine_prompt",
  "repo_path": "/Users/christophehenner/Downloads/Repositories/Playground/Aethyme/Aethyme - Aethyme",
  "task": "Find all public top-level functions in `packages/aethyme/src/indexing/` that are never called from outside that directory.\n\nScope:\n- Check every Python file in `packages/aethyme/src/indexing/` for public top-level function definitions\n- For each public function, search the entire repo outside `packages/aethyme/src/indexing/` for call sites\n- Search at least `packages/aethyme/src/`, `packages/aethyme/tests/`, and `packages/aethyme/scripts/`\n- Exclude private helpers whose names start with `_`\n\nFor each unused function, report:\n- The function name\n- The file it's defined in (relative path)\n- Why you believe it's unused (what you searched for and didn't find)\n\nBe thorough \u2014 check every public top-level function, not just a sample. Missing a truly unused function or falsely flagging a used one both count against you.",
  "focus": "overview",
  "subsystem": "packages/aethyme/src/indexing/",
  "engine_binary": "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/rust/target/release/aethyme-engine-cli"
}
```

### Repo Signals

```json
{}
```

