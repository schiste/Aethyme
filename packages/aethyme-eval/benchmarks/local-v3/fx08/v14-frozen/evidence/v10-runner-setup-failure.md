# FX08 v10 runner setup failure

The v10 frozen package generated 48 invocation attempts. None reached Codex: all exited before creating events.jsonl with `Missing AETHYME_EVAL_OUTPUT_SCHEMA_FILE/AETHYME_EVAL_OUTPUT_SCHEMA`. These are setup failures, not agent runs or pilot scores. The JSON records all 48 attempts; per-attempt driver stderr remains in `/private/tmp/fx08-v10-raw-runs/`.

The v10 freeze and results are preserved at `/private/tmp/fx08-v10-frozen`, tag `fx08-v10-frozen`, revision `aca6407b6ffafa037d578bd8e4ab8d12aa685c2b`. The v11 package adds the required response schema to the frozen run configuration. No task, brief, or oracle was changed.
