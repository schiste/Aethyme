# FX08 v12 runner start failure

All 48 v12 invocations passed the Playground contract and reached the Codex CLI, but none produced a `thread.started` event. The CLI exited before agent work with `failed to initialize in-process app-server client: Operation not permitted`. These are runner-start failures, not task scores. Per-run `events.jsonl`, `stderr.log`, `contract.json`, and driver stderr are retained in `/private/tmp/fx08-v12-raw-runs/`; the v12 target clones remain under `/private/tmp/fx08-v12-playgrounds/`.

A subsequent escalated launch attempt did not invoke Codex because the frozen runner correctly refused to overwrite those v12 paths. V13 freezes fresh roots and records the host-level launcher permission explicitly. The Codex child sandbox remains `workspace-write`.
