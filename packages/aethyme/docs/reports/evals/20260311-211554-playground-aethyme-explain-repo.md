# Eval Report: unknown

Last Updated: 2026-03-11

- Repository: `/Users/christophehenner/Downloads/Repositories/Playground Aethyme`
- Generated: `2026-03-11T21:15:54.847007+00:00`
- Conditions: `control-cto-off, control-cto-on, explore, leverage`

## Summary

- Navigation items: `0`
- Risk items: `0`

### Scorecard

| Condition | Score | Tokens | Tool Calls |
| --- | --- | --- | --- |
| Control (CTO off) | 10.0 | 160K | - |
| Control (CTO on) | 18.33 | 113K | - |
| Explore | 5.0 | 197K | - |
| Leverage | 5.0 | 92K | - |

## Control (CTO off)

### Prompt

```text

```

### Run Metrics

- command: `codex exec -s danger-full-access --output-schema /tmp/aethyme-eval-output-schema.json -o /tmp/aethyme-eval-control-cto-off-result.json`
- exit code: `0`
- input tokens: `160681`
- output tokens: `0`
- retries: `0`
- review burden: `0`
- wall time: `0.000s`

### Final Output Message

```text

```

### Structured Output

```json
null
```

### Raw Run Record

```json
{
  "command": "codex exec -s danger-full-access --output-schema /tmp/aethyme-eval-output-schema.json -o /tmp/aethyme-eval-control-cto-off-result.json",
  "exit_code": 0,
  "input_tokens": 160681,
  "output_tokens": 0,
  "retries": 0,
  "review_burden": 0,
  "duration_seconds": 0.0,
  "tool_calls": null,
  "final_output_message": ""
}
```

### Assessment

```json
{
  "scores": {
    "code_areas": 0.0,
    "reference_areas": 0.0,
    "entrypoints": 0.0,
    "important_docs": 0.0,
    "key_configs": 0.5,
    "key_languages": 0.0,
    "high_risk_areas": 1.0,
    "navigation_order": 0.0,
    "representative_code_files": 0.0,
    "representative_docs": 0.0
  },
  "weighted_score": 10.0,
  "max_score": 100,
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 41,
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 5.0,
    "normalized_score": 10.0,
    "normalization_delta": 5.0
  }
}
```

## Control (CTO on)

### Prompt

```text

```

### Run Metrics

- command: `codex exec -s danger-full-access --output-schema /tmp/aethyme-eval-output-schema.json -o /tmp/aethyme-eval-control-cto-on-result.json`
- exit code: `0`
- input tokens: `113996`
- output tokens: `0`
- retries: `0`
- review burden: `0`
- wall time: `0.000s`

### Final Output Message

```text

```

### Structured Output

```json
null
```

### Raw Run Record

```json
{
  "command": "codex exec -s danger-full-access --output-schema /tmp/aethyme-eval-output-schema.json -o /tmp/aethyme-eval-control-cto-on-result.json",
  "exit_code": 0,
  "input_tokens": 113996,
  "output_tokens": 0,
  "retries": 0,
  "review_burden": 0,
  "duration_seconds": 0.0,
  "tool_calls": null,
  "final_output_message": ""
}
```

### Assessment

```json
{
  "scores": {
    "code_areas": 0.6666666666666666,
    "reference_areas": 0.0,
    "entrypoints": 0.0,
    "important_docs": 0.0,
    "key_configs": 0.0,
    "key_languages": 0.0,
    "high_risk_areas": 1.0,
    "navigation_order": 0.0,
    "representative_code_files": 0.0,
    "representative_docs": 0.0
  },
  "weighted_score": 18.33,
  "max_score": 100,
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 0,
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 18.33,
    "normalized_score": 18.33,
    "normalization_delta": 0.0
  }
}
```

## Explore

### Prompt

```text

```

### Run Metrics

- command: `codex exec -s danger-full-access --output-schema /tmp/aethyme-eval-output-schema.json -o /tmp/aethyme-eval-explore-result.json`
- exit code: `0`
- input tokens: `197923`
- output tokens: `0`
- retries: `0`
- review burden: `0`
- wall time: `0.000s`

### Final Output Message

```text

```

### Structured Output

```json
null
```

### Raw Run Record

```json
{
  "command": "codex exec -s danger-full-access --output-schema /tmp/aethyme-eval-output-schema.json -o /tmp/aethyme-eval-explore-result.json",
  "exit_code": 0,
  "input_tokens": 197923,
  "output_tokens": 0,
  "retries": 0,
  "review_burden": 0,
  "duration_seconds": 0.0,
  "tool_calls": null,
  "final_output_message": ""
}
```

### Assessment

```json
{
  "scores": {
    "code_areas": 0.0,
    "reference_areas": 0.0,
    "entrypoints": 0.0,
    "important_docs": 0.0,
    "key_configs": 0.0,
    "key_languages": 0.0,
    "high_risk_areas": 1.0,
    "navigation_order": 0.0,
    "representative_code_files": 0.0,
    "representative_docs": 0.0
  },
  "weighted_score": 5.0,
  "max_score": 100,
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 0,
      "markdown_link_count": 24,
      "line_anchor_count": 0
    },
    "raw_score": 5.0,
    "normalized_score": 5.0,
    "normalization_delta": 0.0
  }
}
```

## Leverage

### Prompt

```text

```

### Run Metrics

- command: `codex exec -s danger-full-access --output-schema /tmp/aethyme-eval-output-schema.json -o /tmp/aethyme-eval-leverage-result.json`
- exit code: `0`
- input tokens: `92712`
- output tokens: `0`
- retries: `0`
- review burden: `0`
- wall time: `0.000s`

### Final Output Message

```text

```

### Structured Output

```json
null
```

### Raw Run Record

```json
{
  "command": "codex exec -s danger-full-access --output-schema /tmp/aethyme-eval-output-schema.json -o /tmp/aethyme-eval-leverage-result.json",
  "exit_code": 0,
  "input_tokens": 92712,
  "output_tokens": 0,
  "retries": 0,
  "review_burden": 0,
  "duration_seconds": 0.0,
  "tool_calls": null,
  "final_output_message": ""
}
```

### Assessment

```json
{
  "scores": {
    "code_areas": 0.0,
    "reference_areas": 0.0,
    "entrypoints": 0.0,
    "important_docs": 0.0,
    "key_configs": 0.0,
    "key_languages": 0.0,
    "high_risk_areas": 1.0,
    "navigation_order": 0.0,
    "representative_code_files": 0.0,
    "representative_docs": 0.0
  },
  "weighted_score": 5.0,
  "max_score": 100,
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 13,
      "markdown_link_count": 13,
      "line_anchor_count": 0
    },
    "raw_score": 5.0,
    "normalized_score": 5.0,
    "normalization_delta": 0.0
  }
}
```


- Navigation items surfaced: `0`
- Risk items surfaced: `0`

## Reference

### Output Schema

```json
null
```

### Scoring Rubric

```json
null
```

### Reference Output

```json
{
  "repo_summary": "Task: Explain this repo",
  "code_areas": [
    "backend",
    "packages",
    "scripts"
  ],
  "reference_areas": [
    "docs",
    "test-results"
  ],
  "entrypoints": [
    "packages/auth/src/index.ts",
    "packages/config/src/index.ts",
    "packages/types/src/index.ts"
  ],
  "important_docs": [
    "Agents/Skills Manager/README.md",
    "Agents/skills/README.md",
    "Agents/skills/architecture/SKILL.md"
  ],
  "key_configs": [
    "backend/pyproject.toml",
    "packages/auth/package.json"
  ],
  "key_languages": [
    "javascript",
    "python",
    "typescript"
  ],
  "high_risk_areas": [],
  "navigation_order": [
    "Agents/Skills Manager/README.md",
    "Agents/skills/README.md",
    "backend",
    "packages",
    "docs"
  ],
  "representative_code_files": [
    "Agents/skills/_meta/scripts/add_frontmatter.py",
    "Agents/skills/_meta/scripts/analyze_repo.py",
    "Agents/skills/_meta/scripts/analyze_usage_logs.py"
  ],
  "representative_docs": [
    "Agents/Skills Manager/README.md",
    "Agents/skills/README.md",
    "Agents/skills/architecture/SKILL.md"
  ],
  "evidence": [
    "Agents/skills/_meta/scripts/add_frontmatter.py",
    "Agents/skills/_meta/scripts/analyze_repo.py",
    "Agents/Skills Manager/README.md"
  ]
}
```

## Graph Quality Notes

<!-- Post-run analysis of graph quality:
- Did the graph capture the right structural relationships?
- Were important edges missing or spurious?
- How did graph coverage affect each condition's performance?
-->

## Prompt Effectiveness

<!-- Post-run analysis of prompt design:
- Did the control prompt give the agent enough to work with?
- Did the explore prompt's CLI commands get used effectively?
- Did the leverage prompt's context file provide the right framing?
- What prompt changes would improve the next run?
-->

## Lessons & Action Items

<!-- Post-run action items:
- [ ] 
- [ ] 
- [ ] 
-->
