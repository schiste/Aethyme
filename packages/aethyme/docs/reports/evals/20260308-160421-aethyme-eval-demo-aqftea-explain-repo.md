# Eval Report: Explain this repo

Last Updated: 2026-03-08

- Repository: `/tmp/aethyme-eval-demo-AqFTeA`
- Generated: `2026-03-08T16:04:21.307464+00:00`

## Summary

- Baseline prompt chars: `140`
- Aethyme prompt chars: `111`
- Navigation items: `3`
- Risk items: `0`

## Repo Signals

```json
{
  "boundary_clarity": {
    "score": 75,
    "level": "mixed",
    "evidence": [
      "cross-area semantic edges: 0/1",
      "source files with area assignment: 1/1",
      "generic source file names: 0"
    ]
  },
  "entrypoint_clarity": {
    "score": 30,
    "level": "weak",
    "evidence": [
      "direct code entrypoint edges: 0",
      "configs with entrypoints: 0",
      "areas with ambiguous entrypoints: 0"
    ]
  },
  "config_hygiene": {
    "score": 50,
    "level": "weak",
    "evidence": [
      "operational configs: 0",
      "linked configs: 0/0",
      "duplicate config families: 0"
    ]
  },
  "hidden_coupling": {
    "score": 65,
    "level": "mixed",
    "evidence": [
      "low-confidence semantic edges: 0/0",
      "high-confidence semantic edges: 0/0",
      "cross-area semantic edges: 0/0"
    ]
  },
  "parser_visibility": {
    "score": 100,
    "level": "strong",
    "evidence": [
      "supported source files: 1/1",
      "source files with semantic extraction: 1/1",
      "total extracted functions/classes: 1"
    ]
  }
}
```

## Control

### Prompt

```text
Task: Explain this repo
Repository path: /tmp/aethyme-eval-demo-AqFTeA
Explore the repository directly and produce a structured explanation.
```

### Run Metrics

- command: `/opt/homebrew/Cellar/python@3.14/3.14.3_1/Frameworks/Python.framework/Versions/3.14/bin/python3.14 /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/scripts/eval/run_codex_eval.py`
- exit code: `0`
- input tokens: `175458`
- output tokens: `2539`
- retries: `0`
- review burden: `None`
- wall time: `68.411s`

### Final Output Message

```text
{"repo_summary":"Minimal Python demo repository with a single source file and a placeholder README. The repo has one code area (`src`) and one function (`main`) that returns the constant `1`. There is no packaging, no CLI wiring, no tests, and no configuration, so it looks more like a skeleton or evaluation fixture than a complete application.","code_areas":["`src` is the only runtime area. Aethyme’s graph shows the repo contains the `src` area, the `src` directory contains [src/main.py](/private/tmp/aethyme-eval-demo-AqFTeA/src/main.py), and that file defines the only symbol, `main`."],"reference_areas":["[README.md](/private/tmp/aethyme-eval-demo-AqFTeA/README.md) is the only documentation file. It contains only the title `Demo Repo`, so it documents the repo at a very high level but adds no operational detail."],"entrypoints":["The likely entrypoint is [src/main.py](/private/tmp/aethyme-eval-demo-AqFTeA/src/main.py), because it is the only source file and defines the only function. That said, entrypoint clarity is weak: there is no `if __name__ == \"__main__\"`, no console script config, and no framework bootstrap."],"important_docs":["[README.md](/private/tmp/aethyme-eval-demo-AqFTeA/README.md)"],"key_configs":["None found. There are no manifest or operational config files in the repo snapshot Aethyme returned, and direct file listing only showed `README.md` and `src/main.py`."],"key_languages":["Python"],"high_risk_areas":["Functionally, the repo does almost nothing yet. The main risk is ambiguity: consumers cannot tell how `main` is meant to be executed, what `1` represents, or whether this is library code, a script, or a test fixture.","There is no test coverage, no dependency declaration, and no usage documentation."],"navigation_order":["[README.md](/private/tmp/aethyme-eval-demo-AqFTeA/README.md)","`src`","[src/main.py](/private/tmp/aethyme-eval-demo-AqFTeA/src/main.py)"],"representative_code_files":["[src/main.py](/private/tmp/aethyme-eval-demo-AqFTeA/src/main.py)"],"representative_docs":["[README.md](/private/tmp/aethyme-eval-demo-AqFTeA/README.md)"],"evidence":["Aethyme `graph overview` identified one code area (`src`), one source file (`src/main.py`), one function (`main`), one doc (`README.md`), and no configs or explicit entrypoints.","Direct inspection of [src/main.py](/private/tmp/aethyme-eval-demo-AqFTeA/src/main.py) shows: `def main(): return 1`.","Direct inspection of [README.md](/private/tmp/aethyme-eval-demo-AqFTeA/README.md) shows only `# Demo Repo`."]}
```

### Structured Output

```json
{
  "repo_summary": "Minimal Python demo repository with a single source file and a placeholder README. The repo has one code area (`src`) and one function (`main`) that returns the constant `1`. There is no packaging, no CLI wiring, no tests, and no configuration, so it looks more like a skeleton or evaluation fixture than a complete application.",
  "code_areas": [
    "`src` is the only runtime area. Aethyme\u2019s graph shows the repo contains the `src` area, the `src` directory contains [src/main.py](/private/tmp/aethyme-eval-demo-AqFTeA/src/main.py), and that file defines the only symbol, `main`."
  ],
  "reference_areas": [
    "[README.md](/private/tmp/aethyme-eval-demo-AqFTeA/README.md) is the only documentation file. It contains only the title `Demo Repo`, so it documents the repo at a very high level but adds no operational detail."
  ],
  "entrypoints": [
    "The likely entrypoint is [src/main.py](/private/tmp/aethyme-eval-demo-AqFTeA/src/main.py), because it is the only source file and defines the only function. That said, entrypoint clarity is weak: there is no `if __name__ == \"__main__\"`, no console script config, and no framework bootstrap."
  ],
  "important_docs": [
    "[README.md](/private/tmp/aethyme-eval-demo-AqFTeA/README.md)"
  ],
  "key_configs": [
    "None found. There are no manifest or operational config files in the repo snapshot Aethyme returned, and direct file listing only showed `README.md` and `src/main.py`."
  ],
  "key_languages": [
    "Python"
  ],
  "high_risk_areas": [
    "Functionally, the repo does almost nothing yet. The main risk is ambiguity: consumers cannot tell how `main` is meant to be executed, what `1` represents, or whether this is library code, a script, or a test fixture.",
    "There is no test coverage, no dependency declaration, and no usage documentation."
  ],
  "navigation_order": [
    "[README.md](/private/tmp/aethyme-eval-demo-AqFTeA/README.md)",
    "`src`",
    "[src/main.py](/private/tmp/aethyme-eval-demo-AqFTeA/src/main.py)"
  ],
  "representative_code_files": [
    "[src/main.py](/private/tmp/aethyme-eval-demo-AqFTeA/src/main.py)"
  ],
  "representative_docs": [
    "[README.md](/private/tmp/aethyme-eval-demo-AqFTeA/README.md)"
  ],
  "evidence": [
    "Aethyme `graph overview` identified one code area (`src`), one source file (`src/main.py`), one function (`main`), one doc (`README.md`), and no configs or explicit entrypoints.",
    "Direct inspection of [src/main.py](/private/tmp/aethyme-eval-demo-AqFTeA/src/main.py) shows: `def main(): return 1`.",
    "Direct inspection of [README.md](/private/tmp/aethyme-eval-demo-AqFTeA/README.md) shows only `# Demo Repo`."
  ]
}
```

### Raw Run Record

```json
{
  "label": "baseline",
  "command": "/opt/homebrew/Cellar/python@3.14/3.14.3_1/Frameworks/Python.framework/Versions/3.14/bin/python3.14 /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/scripts/eval/run_codex_eval.py",
  "exit_code": 0,
  "duration_seconds": 68.41103779198602,
  "stdout": "{\"input_tokens\": 175458, \"output_tokens\": 2539, \"retries\": 0, \"review_burden\": null, \"final_output_message\": \"{\\\"repo_summary\\\":\\\"Minimal Python demo repository with a single source file and a placeholder README. The repo has one code area (`src`) and one function (`main`) that returns the constant `1`. There is no packaging, no CLI wiring, no tests, and no configuration, so it looks more like a skeleton or evaluation fixture than a complete application.\\\",\\\"code_areas\\\":[\\\"`src` is the only runtime area. Aethyme\\u2019s graph shows the repo contains the `src` area, the `src` directory contains [src/main.py](/private/tmp/aethyme-eval-demo-AqFTeA/src/main.py), and that file defines the only symbol, `main`.\\\"],\\\"reference_areas\\\":[\\\"[README.md](/private/tmp/aethyme-eval-demo-AqFTeA/README.md) is the only documentation file. It contains only the title `Demo Repo`, so it documents the repo at a very high level but adds no operational detail.\\\"],\\\"entrypoints\\\":[\\\"The likely entrypoint is [src/main.py](/private/tmp/aethyme-eval-demo-AqFTeA/src/main.py), because it is the only source file and defines the only function. That said, entrypoint clarity is weak: there is no `if __name__ == \\\\\\\"__main__\\\\\\\"`, no console script config, and no framework bootstrap.\\\"],\\\"important_docs\\\":[\\\"[README.md](/private/tmp/aethyme-eval-demo-AqFTeA/README.md)\\\"],\\\"key_configs\\\":[\\\"None found. There are no manifest or operational config files in the repo snapshot Aethyme returned, and direct file listing only showed `README.md` and `src/main.py`.\\\"],\\\"key_languages\\\":[\\\"Python\\\"],\\\"high_risk_areas\\\":[\\\"Functionally, the repo does almost nothing yet. The main risk is ambiguity: consumers cannot tell how `main` is meant to be executed, what `1` represents, or whether this is library code, a script, or a test fixture.\\\",\\\"There is no test coverage, no dependency declaration, and no usage documentation.\\\"],\\\"navigation_order\\\":[\\\"[README.md](/private/tmp/aethyme-eval-demo-AqFTeA/README.md)\\\",\\\"`src`\\\",\\\"[src/main.py](/private/tmp/aethyme-eval-demo-AqFTeA/src/main.py)\\\"],\\\"representative_code_files\\\":[\\\"[src/main.py](/private/tmp/aethyme-eval-demo-AqFTeA/src/main.py)\\\"],\\\"representative_docs\\\":[\\\"[README.md](/private/tmp/aethyme-eval-demo-AqFTeA/README.md)\\\"],\\\"evidence\\\":[\\\"Aethyme `graph overview` identified one code area (`src`), one source file (`src/main.py`), one function (`main`), one doc (`README.md`), and no configs or explicit entrypoints.\\\",\\\"Direct inspection of [src/main.py](/private/tmp/aethyme-eval-demo-AqFTeA/src/main.py) shows: `def main(): return 1`.\\\",\\\"Direct inspection of [README.md](/private/tmp/aethyme-eval-demo-AqFTeA/README.md) shows only `# Demo Repo`.\\\"]}\", \"structured_output\": {\"repo_summary\": \"Minimal Python demo repository with a single source file and a placeholder README. The repo has one code area (`src`) and one function (`main`) that returns the constant `1`. There is no packaging, no CLI wiring, no tests, and no configuration, so it looks more like a skeleton or evaluation fixture than a complete application.\", \"code_areas\": [\"`src` is the only runtime area. Aethyme\\u2019s graph shows the repo contains the `src` area, the `src` directory contains [src/main.py](/private/tmp/aethyme-eval-demo-AqFTeA/src/main.py), and that file defines the only symbol, `main`.\"], \"reference_areas\": [\"[README.md](/private/tmp/aethyme-eval-demo-AqFTeA/README.md) is the only documentation file. It contains only the title `Demo Repo`, so it documents the repo at a very high level but adds no operational detail.\"], \"entrypoints\": [\"The likely entrypoint is [src/main.py](/private/tmp/aethyme-eval-demo-AqFTeA/src/main.py), because it is the only source file and defines the only function. That said, entrypoint clarity is weak: there is no `if __name__ == \\\"__main__\\\"`, no console script config, and no framework bootstrap.\"], \"important_docs\": [\"[README.md](/private/tmp/aethyme-eval-demo-AqFTeA/README.md)\"], \"key_configs\": [\"None found. There are no manifest or operational config files in the repo snapshot Aethyme returned, and direct file listing only showed `README.md` and `src/main.py`.\"], \"key_languages\": [\"Python\"], \"high_risk_areas\": [\"Functionally, the repo does almost nothing yet. The main risk is ambiguity: consumers cannot tell how `main` is meant to be executed, what `1` represents, or whether this is library code, a script, or a test fixture.\", \"There is no test coverage, no dependency declaration, and no usage documentation.\"], \"navigation_order\": [\"[README.md](/private/tmp/aethyme-eval-demo-AqFTeA/README.md)\", \"`src`\", \"[src/main.py](/private/tmp/aethyme-eval-demo-AqFTeA/src/main.py)\"], \"representative_code_files\": [\"[src/main.py](/private/tmp/aethyme-eval-demo-AqFTeA/src/main.py)\"], \"representative_docs\": [\"[README.md](/private/tmp/aethyme-eval-demo-AqFTeA/README.md)\"], \"evidence\": [\"Aethyme `graph overview` identified one code area (`src`), one source file (`src/main.py`), one function (`main`), one doc (`README.md`), and no configs or explicit entrypoints.\", \"Direct inspection of [src/main.py](/private/tmp/aethyme-eval-demo-AqFTeA/src/main.py) shows: `def main(): return 1`.\", \"Direct inspection of [README.md](/private/tmp/aethyme-eval-demo-AqFTeA/README.md) shows only `# Demo Repo`.\"]}}",
  "stderr": "2026-03-08T16:02:24.618502Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/database (symlink): No such file or directory (os error 2)\n2026-03-08T16:02:24.618521Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/ci-deploy (symlink): No such file or directory (os error 2)\n2026-03-08T16:02:24.618531Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/auth (symlink): No such file or directory (os error 2)\n2026-03-08T16:02:24.618540Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/ai-agents (symlink): No such file or directory (os error 2)\n2026-03-08T16:02:24.618543Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/_meta (symlink): No such file or directory (os error 2)\n2026-03-08T16:02:24.618551Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/observability (symlink): No such file or directory (os error 2)\n2026-03-08T16:02:24.618554Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/integrations (symlink): No such file or directory (os error 2)\n2026-03-08T16:02:24.618557Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/testing (symlink): No such file or directory (os error 2)\n2026-03-08T16:02:24.618560Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/architecture (symlink): No such file or directory (os error 2)\n2026-03-08T16:02:24.618562Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/frontend-quality (symlink): No such file or directory (os error 2)\n2026-03-08T16:02:24.618565Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/agent-workflow (symlink): No such file or directory (os error 2)\n2026-03-08T16:02:24.618568Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/docs-tooling (symlink): No such file or directory (os error 2)\n2026-03-08T16:02:24.618581Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/api (symlink): No such file or directory (os error 2)\n2026-03-08T16:02:24.618588Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/ops (symlink): No such file or directory (os error 2)\n2026-03-08T16:02:24.618596Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/performance (symlink): No such file or directory (os error 2)\n2026-03-08T16:02:24.618599Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/frontend-core (symlink): No such file or directory (os error 2)",
  "input_tokens": 175458,
  "output_tokens": 2539,
  "retries": 0,
  "review_burden": null,
  "final_output_message": "{\"repo_summary\":\"Minimal Python demo repository with a single source file and a placeholder README. The repo has one code area (`src`) and one function (`main`) that returns the constant `1`. There is no packaging, no CLI wiring, no tests, and no configuration, so it looks more like a skeleton or evaluation fixture than a complete application.\",\"code_areas\":[\"`src` is the only runtime area. Aethyme\u2019s graph shows the repo contains the `src` area, the `src` directory contains [src/main.py](/private/tmp/aethyme-eval-demo-AqFTeA/src/main.py), and that file defines the only symbol, `main`.\"],\"reference_areas\":[\"[README.md](/private/tmp/aethyme-eval-demo-AqFTeA/README.md) is the only documentation file. It contains only the title `Demo Repo`, so it documents the repo at a very high level but adds no operational detail.\"],\"entrypoints\":[\"The likely entrypoint is [src/main.py](/private/tmp/aethyme-eval-demo-AqFTeA/src/main.py), because it is the only source file and defines the only function. That said, entrypoint clarity is weak: there is no `if __name__ == \\\"__main__\\\"`, no console script config, and no framework bootstrap.\"],\"important_docs\":[\"[README.md](/private/tmp/aethyme-eval-demo-AqFTeA/README.md)\"],\"key_configs\":[\"None found. There are no manifest or operational config files in the repo snapshot Aethyme returned, and direct file listing only showed `README.md` and `src/main.py`.\"],\"key_languages\":[\"Python\"],\"high_risk_areas\":[\"Functionally, the repo does almost nothing yet. The main risk is ambiguity: consumers cannot tell how `main` is meant to be executed, what `1` represents, or whether this is library code, a script, or a test fixture.\",\"There is no test coverage, no dependency declaration, and no usage documentation.\"],\"navigation_order\":[\"[README.md](/private/tmp/aethyme-eval-demo-AqFTeA/README.md)\",\"`src`\",\"[src/main.py](/private/tmp/aethyme-eval-demo-AqFTeA/src/main.py)\"],\"representative_code_files\":[\"[src/main.py](/private/tmp/aethyme-eval-demo-AqFTeA/src/main.py)\"],\"representative_docs\":[\"[README.md](/private/tmp/aethyme-eval-demo-AqFTeA/README.md)\"],\"evidence\":[\"Aethyme `graph overview` identified one code area (`src`), one source file (`src/main.py`), one function (`main`), one doc (`README.md`), and no configs or explicit entrypoints.\",\"Direct inspection of [src/main.py](/private/tmp/aethyme-eval-demo-AqFTeA/src/main.py) shows: `def main(): return 1`.\",\"Direct inspection of [README.md](/private/tmp/aethyme-eval-demo-AqFTeA/README.md) shows only `# Demo Repo`.\"]}",
  "structured_output": {
    "repo_summary": "Minimal Python demo repository with a single source file and a placeholder README. The repo has one code area (`src`) and one function (`main`) that returns the constant `1`. There is no packaging, no CLI wiring, no tests, and no configuration, so it looks more like a skeleton or evaluation fixture than a complete application.",
    "code_areas": [
      "`src` is the only runtime area. Aethyme\u2019s graph shows the repo contains the `src` area, the `src` directory contains [src/main.py](/private/tmp/aethyme-eval-demo-AqFTeA/src/main.py), and that file defines the only symbol, `main`."
    ],
    "reference_areas": [
      "[README.md](/private/tmp/aethyme-eval-demo-AqFTeA/README.md) is the only documentation file. It contains only the title `Demo Repo`, so it documents the repo at a very high level but adds no operational detail."
    ],
    "entrypoints": [
      "The likely entrypoint is [src/main.py](/private/tmp/aethyme-eval-demo-AqFTeA/src/main.py), because it is the only source file and defines the only function. That said, entrypoint clarity is weak: there is no `if __name__ == \"__main__\"`, no console script config, and no framework bootstrap."
    ],
    "important_docs": [
      "[README.md](/private/tmp/aethyme-eval-demo-AqFTeA/README.md)"
    ],
    "key_configs": [
      "None found. There are no manifest or operational config files in the repo snapshot Aethyme returned, and direct file listing only showed `README.md` and `src/main.py`."
    ],
    "key_languages": [
      "Python"
    ],
    "high_risk_areas": [
      "Functionally, the repo does almost nothing yet. The main risk is ambiguity: consumers cannot tell how `main` is meant to be executed, what `1` represents, or whether this is library code, a script, or a test fixture.",
      "There is no test coverage, no dependency declaration, and no usage documentation."
    ],
    "navigation_order": [
      "[README.md](/private/tmp/aethyme-eval-demo-AqFTeA/README.md)",
      "`src`",
      "[src/main.py](/private/tmp/aethyme-eval-demo-AqFTeA/src/main.py)"
    ],
    "representative_code_files": [
      "[src/main.py](/private/tmp/aethyme-eval-demo-AqFTeA/src/main.py)"
    ],
    "representative_docs": [
      "[README.md](/private/tmp/aethyme-eval-demo-AqFTeA/README.md)"
    ],
    "evidence": [
      "Aethyme `graph overview` identified one code area (`src`), one source file (`src/main.py`), one function (`main`), one doc (`README.md`), and no configs or explicit entrypoints.",
      "Direct inspection of [src/main.py](/private/tmp/aethyme-eval-demo-AqFTeA/src/main.py) shows: `def main(): return 1`.",
      "Direct inspection of [README.md](/private/tmp/aethyme-eval-demo-AqFTeA/README.md) shows only `# Demo Repo`."
    ]
  }
}
```

### Assessment

```json
{
  "scores": {
    "code_areas": 0.0,
    "reference_areas": 1.0,
    "entrypoints": 1.0,
    "important_docs": 0.0,
    "key_configs": 1.0,
    "key_languages": 0.0,
    "high_risk_areas": 1.0,
    "navigation_order": 0.0,
    "representative_code_files": 0.0,
    "representative_docs": 0.0
  },
  "weighted_score": 45.0,
  "max_score": 100
}
```

## Aethyme

### Prompt

```text
Task: Explain this repo
Use `AETHYME_EVAL_NAVIGATION_CONTEXT_FILE`.
Return only the required structured output.
```

### Run Metrics

- command: `/opt/homebrew/Cellar/python@3.14/3.14.3_1/Frameworks/Python.framework/Versions/3.14/bin/python3.14 /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/scripts/eval/run_codex_eval.py`
- exit code: `0`
- input tokens: `83221`
- output tokens: `1907`
- retries: `0`
- review burden: `None`
- wall time: `48.361s`

### Final Output Message

```text
{"repo_summary":"Minimal Python demo repo. Aethyme shows one code area (`src`), one source file (`src/main.py`), and one README. The only extracted symbol is `src/main.py::main`, which returns `1`, so this is effectively a very small sample repo with implicit runtime intent rather than a fully wired application.","code_areas":["src"],"reference_areas":["README.md"],"entrypoints":["src/main.py::main"],"important_docs":["README.md"],"key_configs":[],"key_languages":["python"],"high_risk_areas":["src/main.py"],"navigation_order":["README.md","src","src/main.py"],"representative_code_files":["src/main.py"],"representative_docs":["README.md"],"evidence":["AETHYME_EVAL_NAVIGATION_CONTEXT_FILE scoped the explanation to `README.md`, `src`, and `src/main.py`.","Aethyme `repo inspect` found exactly two files: `README.md` and `src/main.py`.","Aethyme `graph overview` found one code area, `src`, and no configs.","Aethyme graph edges show `repo -> src area -> src/main.py`.","Aethyme `graph expand src/main.py` shows that file defines the only extracted symbol, `src/main.py::main`.","Direct file reads confirm `README.md` only contains the title `Demo Repo` and `src/main.py` contains `def main(): return 1`."]}
```

### Structured Output

```json
{
  "repo_summary": "Minimal Python demo repo. Aethyme shows one code area (`src`), one source file (`src/main.py`), and one README. The only extracted symbol is `src/main.py::main`, which returns `1`, so this is effectively a very small sample repo with implicit runtime intent rather than a fully wired application.",
  "code_areas": [
    "src"
  ],
  "reference_areas": [
    "README.md"
  ],
  "entrypoints": [
    "src/main.py::main"
  ],
  "important_docs": [
    "README.md"
  ],
  "key_configs": [],
  "key_languages": [
    "python"
  ],
  "high_risk_areas": [
    "src/main.py"
  ],
  "navigation_order": [
    "README.md",
    "src",
    "src/main.py"
  ],
  "representative_code_files": [
    "src/main.py"
  ],
  "representative_docs": [
    "README.md"
  ],
  "evidence": [
    "AETHYME_EVAL_NAVIGATION_CONTEXT_FILE scoped the explanation to `README.md`, `src`, and `src/main.py`.",
    "Aethyme `repo inspect` found exactly two files: `README.md` and `src/main.py`.",
    "Aethyme `graph overview` found one code area, `src`, and no configs.",
    "Aethyme graph edges show `repo -> src area -> src/main.py`.",
    "Aethyme `graph expand src/main.py` shows that file defines the only extracted symbol, `src/main.py::main`.",
    "Direct file reads confirm `README.md` only contains the title `Demo Repo` and `src/main.py` contains `def main(): return 1`."
  ]
}
```

### Raw Run Record

```json
{
  "label": "aethyme",
  "command": "/opt/homebrew/Cellar/python@3.14/3.14.3_1/Frameworks/Python.framework/Versions/3.14/bin/python3.14 /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/scripts/eval/run_codex_eval.py",
  "exit_code": 0,
  "duration_seconds": 48.36119370802771,
  "stdout": "{\"input_tokens\": 83221, \"output_tokens\": 1907, \"retries\": 0, \"review_burden\": null, \"final_output_message\": \"{\\\"repo_summary\\\":\\\"Minimal Python demo repo. Aethyme shows one code area (`src`), one source file (`src/main.py`), and one README. The only extracted symbol is `src/main.py::main`, which returns `1`, so this is effectively a very small sample repo with implicit runtime intent rather than a fully wired application.\\\",\\\"code_areas\\\":[\\\"src\\\"],\\\"reference_areas\\\":[\\\"README.md\\\"],\\\"entrypoints\\\":[\\\"src/main.py::main\\\"],\\\"important_docs\\\":[\\\"README.md\\\"],\\\"key_configs\\\":[],\\\"key_languages\\\":[\\\"python\\\"],\\\"high_risk_areas\\\":[\\\"src/main.py\\\"],\\\"navigation_order\\\":[\\\"README.md\\\",\\\"src\\\",\\\"src/main.py\\\"],\\\"representative_code_files\\\":[\\\"src/main.py\\\"],\\\"representative_docs\\\":[\\\"README.md\\\"],\\\"evidence\\\":[\\\"AETHYME_EVAL_NAVIGATION_CONTEXT_FILE scoped the explanation to `README.md`, `src`, and `src/main.py`.\\\",\\\"Aethyme `repo inspect` found exactly two files: `README.md` and `src/main.py`.\\\",\\\"Aethyme `graph overview` found one code area, `src`, and no configs.\\\",\\\"Aethyme graph edges show `repo -> src area -> src/main.py`.\\\",\\\"Aethyme `graph expand src/main.py` shows that file defines the only extracted symbol, `src/main.py::main`.\\\",\\\"Direct file reads confirm `README.md` only contains the title `Demo Repo` and `src/main.py` contains `def main(): return 1`.\\\"]}\", \"structured_output\": {\"repo_summary\": \"Minimal Python demo repo. Aethyme shows one code area (`src`), one source file (`src/main.py`), and one README. The only extracted symbol is `src/main.py::main`, which returns `1`, so this is effectively a very small sample repo with implicit runtime intent rather than a fully wired application.\", \"code_areas\": [\"src\"], \"reference_areas\": [\"README.md\"], \"entrypoints\": [\"src/main.py::main\"], \"important_docs\": [\"README.md\"], \"key_configs\": [], \"key_languages\": [\"python\"], \"high_risk_areas\": [\"src/main.py\"], \"navigation_order\": [\"README.md\", \"src\", \"src/main.py\"], \"representative_code_files\": [\"src/main.py\"], \"representative_docs\": [\"README.md\"], \"evidence\": [\"AETHYME_EVAL_NAVIGATION_CONTEXT_FILE scoped the explanation to `README.md`, `src`, and `src/main.py`.\", \"Aethyme `repo inspect` found exactly two files: `README.md` and `src/main.py`.\", \"Aethyme `graph overview` found one code area, `src`, and no configs.\", \"Aethyme graph edges show `repo -> src area -> src/main.py`.\", \"Aethyme `graph expand src/main.py` shows that file defines the only extracted symbol, `src/main.py::main`.\", \"Direct file reads confirm `README.md` only contains the title `Demo Repo` and `src/main.py` contains `def main(): return 1`.\"]}}",
  "stderr": "2026-03-08T16:03:33.047357Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/database (symlink): No such file or directory (os error 2)\n2026-03-08T16:03:33.047379Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/ci-deploy (symlink): No such file or directory (os error 2)\n2026-03-08T16:03:33.047390Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/auth (symlink): No such file or directory (os error 2)\n2026-03-08T16:03:33.047400Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/ai-agents (symlink): No such file or directory (os error 2)\n2026-03-08T16:03:33.047403Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/_meta (symlink): No such file or directory (os error 2)\n2026-03-08T16:03:33.047412Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/observability (symlink): No such file or directory (os error 2)\n2026-03-08T16:03:33.047415Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/integrations (symlink): No such file or directory (os error 2)\n2026-03-08T16:03:33.047418Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/testing (symlink): No such file or directory (os error 2)\n2026-03-08T16:03:33.047421Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/architecture (symlink): No such file or directory (os error 2)\n2026-03-08T16:03:33.047424Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/frontend-quality (symlink): No such file or directory (os error 2)\n2026-03-08T16:03:33.047426Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/agent-workflow (symlink): No such file or directory (os error 2)\n2026-03-08T16:03:33.047429Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/docs-tooling (symlink): No such file or directory (os error 2)\n2026-03-08T16:03:33.047441Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/api (symlink): No such file or directory (os error 2)\n2026-03-08T16:03:33.047448Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/ops (symlink): No such file or directory (os error 2)\n2026-03-08T16:03:33.047456Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/performance (symlink): No such file or directory (os error 2)\n2026-03-08T16:03:33.047459Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/frontend-core (symlink): No such file or directory (os error 2)",
  "input_tokens": 83221,
  "output_tokens": 1907,
  "retries": 0,
  "review_burden": null,
  "final_output_message": "{\"repo_summary\":\"Minimal Python demo repo. Aethyme shows one code area (`src`), one source file (`src/main.py`), and one README. The only extracted symbol is `src/main.py::main`, which returns `1`, so this is effectively a very small sample repo with implicit runtime intent rather than a fully wired application.\",\"code_areas\":[\"src\"],\"reference_areas\":[\"README.md\"],\"entrypoints\":[\"src/main.py::main\"],\"important_docs\":[\"README.md\"],\"key_configs\":[],\"key_languages\":[\"python\"],\"high_risk_areas\":[\"src/main.py\"],\"navigation_order\":[\"README.md\",\"src\",\"src/main.py\"],\"representative_code_files\":[\"src/main.py\"],\"representative_docs\":[\"README.md\"],\"evidence\":[\"AETHYME_EVAL_NAVIGATION_CONTEXT_FILE scoped the explanation to `README.md`, `src`, and `src/main.py`.\",\"Aethyme `repo inspect` found exactly two files: `README.md` and `src/main.py`.\",\"Aethyme `graph overview` found one code area, `src`, and no configs.\",\"Aethyme graph edges show `repo -> src area -> src/main.py`.\",\"Aethyme `graph expand src/main.py` shows that file defines the only extracted symbol, `src/main.py::main`.\",\"Direct file reads confirm `README.md` only contains the title `Demo Repo` and `src/main.py` contains `def main(): return 1`.\"]}",
  "structured_output": {
    "repo_summary": "Minimal Python demo repo. Aethyme shows one code area (`src`), one source file (`src/main.py`), and one README. The only extracted symbol is `src/main.py::main`, which returns `1`, so this is effectively a very small sample repo with implicit runtime intent rather than a fully wired application.",
    "code_areas": [
      "src"
    ],
    "reference_areas": [
      "README.md"
    ],
    "entrypoints": [
      "src/main.py::main"
    ],
    "important_docs": [
      "README.md"
    ],
    "key_configs": [],
    "key_languages": [
      "python"
    ],
    "high_risk_areas": [
      "src/main.py"
    ],
    "navigation_order": [
      "README.md",
      "src",
      "src/main.py"
    ],
    "representative_code_files": [
      "src/main.py"
    ],
    "representative_docs": [
      "README.md"
    ],
    "evidence": [
      "AETHYME_EVAL_NAVIGATION_CONTEXT_FILE scoped the explanation to `README.md`, `src`, and `src/main.py`.",
      "Aethyme `repo inspect` found exactly two files: `README.md` and `src/main.py`.",
      "Aethyme `graph overview` found one code area, `src`, and no configs.",
      "Aethyme graph edges show `repo -> src area -> src/main.py`.",
      "Aethyme `graph expand src/main.py` shows that file defines the only extracted symbol, `src/main.py::main`.",
      "Direct file reads confirm `README.md` only contains the title `Demo Repo` and `src/main.py` contains `def main(): return 1`."
    ]
  }
}
```

### Assessment

```json
{
  "scores": {
    "code_areas": 0.0,
    "reference_areas": 1.0,
    "entrypoints": 1.0,
    "important_docs": 0.0,
    "key_configs": 1.0,
    "key_languages": 0.0,
    "high_risk_areas": 1.0,
    "navigation_order": 0.0,
    "representative_code_files": 0.0,
    "representative_docs": 0.0
  },
  "weighted_score": 45.0,
  "max_score": 100
}
```

## Comparison

- Prompt chars delta: `-29`
- Navigation items surfaced: `3`
- Risk items surfaced: `0`

## Reference

### Output Schema

```json
{
  "type": "object",
  "additionalProperties": false,
  "required": [
    "repo_summary",
    "code_areas",
    "reference_areas",
    "entrypoints",
    "important_docs",
    "key_configs",
    "key_languages",
    "high_risk_areas",
    "navigation_order",
    "representative_code_files",
    "representative_docs",
    "evidence"
  ],
  "properties": {
    "repo_summary": {
      "type": "string"
    },
    "code_areas": {
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "reference_areas": {
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "entrypoints": {
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "important_docs": {
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "key_configs": {
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "key_languages": {
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "high_risk_areas": {
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "navigation_order": {
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "representative_code_files": {
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "representative_docs": {
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "evidence": {
      "type": "array",
      "items": {
        "type": "string"
      }
    }
  }
}
```

### Scoring Rubric

```json
{
  "weights": {
    "code_areas": 20,
    "reference_areas": 10,
    "entrypoints": 20,
    "important_docs": 15,
    "key_configs": 10,
    "key_languages": 10,
    "high_risk_areas": 5,
    "navigation_order": 5,
    "representative_code_files": 3,
    "representative_docs": 2
  },
  "notes": [
    "Prefer exact path and area matches.",
    "Navigation order is partial-credit and ordered.",
    "Repo summary is informative but not currently machine-scored."
  ]
}
```

### Reference Output

```json
{
  "repo_summary": "Task: Explain this repo",
  "code_areas": [
    "src"
  ],
  "reference_areas": [],
  "entrypoints": [],
  "important_docs": [
    "README.md"
  ],
  "key_configs": [],
  "key_languages": [
    "python"
  ],
  "high_risk_areas": [],
  "navigation_order": [
    "README.md",
    "src"
  ],
  "representative_code_files": [
    "src/main.py"
  ],
  "representative_docs": [
    "README.md"
  ],
  "evidence": [
    "src/main.py",
    "README.md"
  ]
}
```

## Navigation Context

```json
{
  "mode": "iterative_navigation",
  "repo_path": "/tmp/aethyme-eval-demo-AqFTeA",
  "tool_repo_path": "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme",
  "tool_python": "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python",
  "task": "Explain this repo",
  "anchors": {
    "task": "Explain this repo",
    "anchors": [
      {
        "kind": "file",
        "id": "README.md",
        "file": "README.md",
        "reason": "repository readme"
      },
      {
        "kind": "folder",
        "id": "src",
        "file": null,
        "reason": "top-level area"
      },
      {
        "kind": "file",
        "id": "src/main.py",
        "file": "src/main.py",
        "reason": "likely entrypoint"
      }
    ]
  },
  "scope": {
    "task": "Explain this repo",
    "navigation_order": [
      "README.md",
      "src",
      "src/main.py"
    ],
    "in_scope_files": [
      "src/main.py"
    ],
    "in_scope_symbols": [
      "src/main.py::main"
    ],
    "in_scope_areas": [
      "src"
    ],
    "out_of_scope": [],
    "risks": []
  },
  "commands": [
    "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli repo inspect /tmp/aethyme-eval-demo-AqFTeA --json-output",
    "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph overview /tmp/aethyme-eval-demo-AqFTeA --json-output",
    "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph expand /tmp/aethyme-eval-demo-AqFTeA <anchor-id> --json-output"
  ]
}
```

## Aethyme Pack

```json
{
  "task": {
    "raw": "Explain this repo",
    "normalized": "explain this repo",
    "kind": "explain_repo"
  },
  "overview": {
    "overview_docs": [
      "README.md"
    ],
    "code_areas": [
      "src"
    ],
    "reference_areas": [],
    "subareas": [],
    "entrypoints": [],
    "key_configs": [],
    "representative_code_files": [
      "src/main.py"
    ],
    "representative_docs": [
      "README.md"
    ]
  },
  "anchors": [
    {
      "kind": "file",
      "id": "README.md",
      "file": "README.md",
      "reason": "repository readme"
    },
    {
      "kind": "folder",
      "id": "src",
      "file": null,
      "reason": "top-level area"
    },
    {
      "kind": "file",
      "id": "src/main.py",
      "file": "src/main.py",
      "reason": "likely entrypoint"
    }
  ],
  "in_scope": {
    "files": [
      {
        "value": "src/main.py",
        "kind": "file",
        "reason": "anchor-adjacent file"
      }
    ],
    "symbols": [
      {
        "value": "src/main.py::main",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      }
    ],
    "areas": [
      {
        "value": "src",
        "kind": "area",
        "reason": "primary top-level area"
      }
    ]
  },
  "out_of_scope": {
    "files": [],
    "symbols": [],
    "areas": []
  },
  "dependencies": [
    {
      "from": "dir:aethyme-eval-demo-AqFTeA:src",
      "to": "src/main.py",
      "kind": "contains"
    },
    {
      "from": "src",
      "to": "dir:aethyme-eval-demo-AqFTeA:src",
      "kind": "contains"
    },
    {
      "from": "src/main.py",
      "to": "src/main.py::main",
      "kind": "defines"
    }
  ],
  "impact": [],
  "snippets": [
    {
      "file": "README.md",
      "start_line": 1,
      "end_line": 1,
      "kind": "overview"
    },
    {
      "file": "src/main.py",
      "start_line": 1,
      "end_line": 2,
      "kind": "overview"
    }
  ],
  "risk_flags": [],
  "navigation_order": [
    "README.md",
    "src",
    "src/main.py"
  ],
  "budget": {
    "max_anchors": 5,
    "max_files": 8,
    "max_snippets": 8,
    "dependency_depth": 1,
    "impact_depth": 1
  },
  "confidence": {
    "anchor_confidence": 0.85,
    "scope_confidence": 0.8
  }
}
```

## Explanation

```text
Task: Explain this repo
Languages: python
Top-level directories: src
Files indexed: 2
Functions indexed: 1
Classes indexed: 0
Docs indexed: 1
Configs indexed: 0
README: README.md

Code areas:
- src

Representative code:
- src/main.py

Representative docs:
- README.md

Navigation order:
- README.md
- src
- src/main.py
```
