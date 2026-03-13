# Eval Report: Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.

Last Updated: 2026-03-09

- Repository: `/Users/christophehenner/Downloads/Repositories/Aethyme Playground`
- Generated: `2026-03-09T11:29:44.996011+00:00`

## Summary

- Control prompt chars: `322`
- Explore prompt chars: `1633`
- Leverage prompt chars: `257`
- Navigation items: `1`
- Risk items: `961`

## Repo Signals

```json
{
  "boundary_clarity": {
    "score": 68,
    "level": "mixed",
    "evidence": [
      "cross-area semantic edges: 41827/264266",
      "source files with area assignment: 5350/5369",
      "generic source file names: 13"
    ]
  },
  "entrypoint_clarity": {
    "score": 100,
    "level": "strong",
    "evidence": [
      "direct code entrypoint edges: 1192",
      "configs with entrypoints: 6",
      "areas with ambiguous entrypoints: 1"
    ]
  },
  "config_hygiene": {
    "score": 21,
    "level": "weak",
    "evidence": [
      "operational configs: 39",
      "linked configs: 39/39",
      "duplicate config families: 26"
    ]
  },
  "hidden_coupling": {
    "score": 23,
    "level": "weak",
    "evidence": [
      "low-confidence semantic edges: 209164/235410",
      "high-confidence semantic edges: 14639/235410",
      "cross-area semantic edges: 30948/235410"
    ]
  },
  "parser_visibility": {
    "score": 87,
    "level": "strong",
    "evidence": [
      "supported source files: 5099/5369",
      "source files with semantic extraction: 3816/5369",
      "total extracted functions/classes: 16018"
    ]
  }
}
```

## Control

### Prompt

```text
Task: Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.
Repository path: /Users/christophehenner/Downloads/Repositories/Aethyme Playground
Explore the repository directly and produce a structured explanation.
```

### Run Metrics

- input tokens: `null`
- output tokens: `null`
- retries: `null`
- wall time: `null`

### Final Output Message

```text
Control runner not executed.
```

### Structured Output

```json
null
```

## Explore

### Prompt

```text
Task: Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.
Repository path: /Users/christophehenner/Downloads/Repositories/Aethyme Playground
Explore the repository and produce a structured explanation.

You have access to the following graph navigation commands.
Use them via Bash to explore the repository graph:

  cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli task anchors --repo '/Users/christophehenner/Downloads/Repositories/Aethyme Playground' --task <task> --json-output
  cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli task scope --repo '/Users/christophehenner/Downloads/Repositories/Aethyme Playground' --task <task> --json-output
  cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph configs '/Users/christophehenner/Downloads/Repositories/Aethyme Playground' packages --json-output
  cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph expand '/Users/christophehenner/Downloads/Repositories/Aethyme Playground' <anchor-id> --json-output

Return only the required structured output.
```

### Run Metrics

- input tokens: `null`
- output tokens: `null`
- retries: `null`
- wall time: `null`

### Final Output Message

```text
Explore runner not executed.
```

### Structured Output

```json
null
```

## Leverage

### Prompt

```text
Task: Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.
Use `AETHYME_EVAL_NAVIGATION_CONTEXT_FILE`.
Return only the required structured output.
```

### Run Metrics

- input tokens: `null`
- output tokens: `null`
- retries: `null`
- wall time: `null`

### Final Output Message

```text
Leverage runner not executed.
```

### Structured Output

```json
null
```


## Context Pack Audit

### Pack Summary

- Anchors: `2`
- Navigation order items: `1`
- In-scope files: `0`
- CLI commands: `4`

### Navigation Context

```json
{
  "mode": "iterative_navigation",
  "repo_path": "/Users/christophehenner/Downloads/Repositories/Aethyme Playground",
  "tool_repo_path": "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme",
  "tool_python": "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python",
  "task": "Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.",
  "challenge": {
    "kind": "navigation_ctf",
    "task": "Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.",
    "reference_output": {
      "config_target": {
        "path": "packages/auth/package.json",
        "why": "manifest/config linked to the runtime entrypoint"
      },
      "code_target": {
        "path": "packages/ui/src/tokens/index.ts",
        "why": "entrypoint file linked by the configuration graph"
      },
      "management_area": {
        "name": "packages",
        "why": "top-level area linked by the configuration graph"
      },
      "relationship_chain": [
        {
          "from": "packages/auth/package.json",
          "to": "packages",
          "relation": "configures"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/api/mapping-intelligence/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/auth/ability.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/auth/can.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/auth/permissionGrouping.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/auth/rbac-canonical.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/auth/session.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/components/admin/adn/local-entries/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/components/admin/adn/taxonomy/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/components/admin/db/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/components/adn/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/components/adn/widgets/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/components/chrome/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/components/chrome/layout/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/components/collaboration/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/components/domain/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/components/information/ComplianceStudio/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/components/information/PolicyStudio/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/components/information/shared/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/components/integrity/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/components/layout/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/components/mapping-intelligence/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/components/suppliers/assessments/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/components/suppliers/changes/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/components/suppliers/comments/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/components/suppliers/exceptions/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/components/suppliers/incidents/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/components/suppliers/signals/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/config/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/config/page-actions/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/admin/components/analytics/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/admin/components/assignments/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/admin/components/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/admin/components/integrations/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/admin/components/menu-manager/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/admin/components/risk-rules/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/admin/components/users/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/admin/hooks/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/admin/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/admin/pages/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/admin/schemas/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/admin/types/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/auth/components/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/auth/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/auth/utils/ability.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/auth/utils/can.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/auth/utils/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/auth/utils/permissionGrouping.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/auth/utils/session.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/automations/components/builder-v2/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/automations/components/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/automations/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/automations/pages/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/bulk/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/campaigns/components/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/campaigns/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/campaigns/pages/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/collaboration/components/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/collaboration/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/collaboration/pages/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/controls/components/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/controls/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/controls/pages/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/controls/schemas/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/controls/types/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/environment/components/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/environment/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/environment/pages/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/environment/schemas/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/information/components/ComplianceStudio/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/information/components/PolicyStudio/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/information/components/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/information/components/shared/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/information/hooks/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/information/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/information/pages/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/notifications/components/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/notifications/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/notifications/pages/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/notifications/schemas/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/notifications/types/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/onboarding2/__stories__/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/onboarding2/components/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/onboarding2/components/steps/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/onboarding2/hooks/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/onboarding2/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/org/pages/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/platform/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/platform/pages/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/policy/components/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/policy/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/profile/components/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/profile/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/profile/pages/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/reporting/components/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/reporting/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/reporting/schemas/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/risk/components/builder/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/risk/components/builder/inspectors/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/risk/components/builder/nodes/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/risk/components/calculator/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/risk/components/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/risk/components/manager/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/risk/components/shared/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/risk/hooks/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/risk/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/risk/pages/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/risk/services/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/risk/types/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/risk/utils/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/suppliers/components/assessments/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/suppliers/components/changes/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/suppliers/components/comments/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/suppliers/components/exceptions/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/suppliers/components/incidents/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/suppliers/components/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/suppliers/components/signals/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/suppliers/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/suppliers/pages/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/suppliers/schemas/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/suppliers/types/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/tasks/components/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/features/tasks/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/hooks/admin/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/hooks/chrome/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/hooks/collaboration/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/hooks/environment/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/hooks/information/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/hooks/menu/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/hooks/permissions/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/lib/api/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/lib/automations/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/lib/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/preauth/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/preauth/session.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/providers/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/schemas/api/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/schemas/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/shared/hooks/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/shared/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/stores/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/types/api/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/types/automations/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/app-shared/src/types/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/auth/src/ability.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/auth/src/can.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/auth/src/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/auth/src/logout/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/auth/src/permissionGrouping.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/auth/src/rbac-canonical.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/auth/src/session.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/config/src/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/eslint-plugin-aeptus/src/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/types/src/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/types/src/session.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/Avatar/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/BackButton/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/Badge/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/Button/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/Card/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/Checkbox/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/DataSourceBadge/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/Divider/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/DropZone/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/HelpTrigger/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/Icon/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/IconButton/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/Input/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/LazyImage/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/Logo/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/ManagedChip/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/NumericStepper/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/OptionList/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/PhoneInput/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/ScopeRiskBadge/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/ScrollArea/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/SemanticBadge/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/Skeleton/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/Spinner/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/StatusIndicator/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/Switch/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/Table/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/Textarea/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/TierBadge/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/ToolbarSelect/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/atoms/Tooltip/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/charts/AreaChart/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/charts/BarChart/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/charts/ChartCard/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/charts/KpiWidget/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/charts/LineChart/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/charts/PieChart/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/charts/Sparkline/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/Accordion/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/ActionBar/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/ActionForm/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/Alert/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/AlertDialog/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/AsyncCombobox/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/AvatarUpload/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/Breadcrumbs/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/CodeSnippet/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/Collapsible/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/ColorPicker/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/Combobox/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/ComboboxWithRefs/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/Command/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/ConfirmDialog/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/DatePicker/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/DateRangePicker/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/DateTimePicker/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/DescriptionList/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/DialogSection/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/Drawer/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/DropdownMenu/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/EmptyState/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/FieldWrapper/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/FloatingPanel/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/InlineEdit/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/Input/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/Label/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/Modal/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/MultiSelect/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/MultiTypeCombobox/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/OverlayWidget/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/OwnerSelectBadge/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/Pagination/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/Popover/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/ProgressBar/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/PullToRefresh/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/RHFForm/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/RadioGroup/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/RangeSlider/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/SavedViews/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/SearchInput/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/Section/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/SegmentedControl/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/Select/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/SensitiveValueDisplay/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/ServerPagination/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/Sheet/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/Skeleton/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/Skeleton/variants/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/Slider/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/StatusBanner/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/Stepper/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/Switch/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/TableSortHeader/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/Tabs/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/Toast/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/ToastContainer/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/TokenPicker/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/Toolbar/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/Tooltip/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/ValidationSummary/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/molecules/ViewToggle/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/organisms/Carousel/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/organisms/ColumnVisibilityMenu/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/organisms/CommandPalette/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/organisms/DataTable/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/organisms/FileUpload/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/organisms/Form/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/organisms/RichTextEditor/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/organisms/Sidebar/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/BulkActionsToolbar/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/FilterBuilder/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/IntegrationCard/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/compositions/ActionBar/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/compositions/ActivityFeed/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/compositions/BulkEditDialog/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/compositions/CardFilter/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/compositions/CollectionView/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/compositions/EnhancedSearchBar/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/compositions/ExportDialog/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/compositions/FacetFilter/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/compositions/FilterBar/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/compositions/FilterPanel/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/compositions/KanbanBoard/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/compositions/LogicBuilder/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/compositions/ModalFilter/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/compositions/NotificationCenter/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/compositions/RangeSliderFilter/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/compositions/Timeline/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/compositions/ToggleFilter/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/compositions/UserMenu/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/flows/OnboardingTooltip/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/flows/OnboardingTooltips/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/flows/Wizard/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/templates/DashboardLayout/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/templates/DetailPage/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/templates/FormPage/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/templates/ListPage/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/templates/MasterDetail/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/templates/ResourceLayout/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/templates/SettingsPage/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/patterns/templates/WizardPage/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/utilities/VirtualizedTableBody/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/components/utilities/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/hooks/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/index.ts",
          "relation": "entrypoint_for"
        },
        {
          "from": "packages/auth/package.json",
          "to": "packages/ui/src/tokens/index.ts",
          "relation": "entrypoint_for"
        }
      ],
      "rejected_candidates": [],
      "confidence": "high"
    }
  },
  "anchors": {
    "task": "Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.",
    "anchors": [
      {
        "kind": "folder",
        "id": ".claude/commands",
        "file": null,
        "reason": "area match"
      }
    ]
  },
  "scope": {
    "task": "Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.",
    "navigation_order": [
      ".claude/commands"
    ],
    "in_scope_files": [],
    "in_scope_symbols": [],
    "in_scope_areas": [
      ".claude/commands"
    ],
    "out_of_scope": [
      ".chau7",
      ".chunk-history",
      ".claude",
      ".gcloud_access_token",
      ".gcloud_tmp",
      ".githooks",
      ".github",
      ".github/PULL_REQUEST_TEMPLATE",
      ".github/workflows",
      ".github/workflows/migrations-guard.yml",
      ".husky",
      ".hypothesis",
      ".lighthouseci",
      ".playwright-mcp",
      ".pnpm-store",
      ".pnpm-store/v10/index/17/3659f9b86de57d0529eeccc33dc3015026947d415796e549a93f9473012b3d-oauth4webapi@3.8.2.json",
      ".pnpm-store/v10/index/18/7b8344ed764b2a6ed9c57bd1dd5d900d845265c7827b6bcdba6f381f90cbee-comma-separated-tokens@1.0.8.json",
      ".pnpm-store/v10/index/29/afbd4ebbadbfb1bc33a593e927a2456cfbf762b9a84a881841b35ca84013ac-class-variance-authority@0.7.1.json",
      ".pnpm-store/v10/index/45/d2547e5704ddc5332a232a420b02bb4e853eef5474824ed1b7986cf8473789-js-tokens@4.0.0.json",
      ".pnpm-store/v10/index/55/dffd1150e2bba3cf26df72021eaba193fa125d711eb76f2151a3c81b074744-@csstools+css-tokenizer@3.0.4.json",
      ".pnpm-store/v10/index/59/dee61cf43ff33cba423edfe13e3abe0ddaa28afc7ec9099ba8366728f4eb8a-@auth+core@0.41.0.json",
      ".pnpm-store/v10/index/9b/16bd13d21314eb746da9f78fa2f93298f07a01b3ea505098cd4826459e0591-js-tokens@9.0.1.json",
      ".pnpm-store/v10/index/a3/69ee27ce43e04491c9b877cdb0390e5d4e7b5edf4592fefd0d7b6f5a90752f-@auth0+auth0-react@2.5.0.json",
      ".pnpm-store/v10/index/ab/f25255dd4ba6dce17f96e4626e286f88963e3c742a245edec44504dad5a9b2-space-separated-tokens@1.1.5.json",
      ".pnpm-store/v10/index/e1/7bf1d84e0dd808abaf5469f8a39e8dd0dba63e4b9df2ed359fd368e768ed56-@auth0+auth0-spa-js@2.5.0.json",
      ".pnpm-store/v10/index/f9/ce7582ab8cdc5ea73159a802eb1127b448a18d0ae13b3d1c20b0cb2fc14687-next-auth@5.0.0-beta.30.json",
      ".pnpm-store/v10/index/ff/b05db84885788349ee695cf22466aa9d2c0f0d9ada50056a18a0fd11a9a67e-eslint-plugin-no-secrets@2.2.1.json",
      ".secrets.baseline",
      ".storybook",
      ".wrangler",
      "Agents",
      "Agents/Skills Manager",
      "Agents/skills",
      "Agents/skills/auth/SKILL.md",
      "Agents/skills/auth/references/api-endpoints.md",
      "Agents/skills/auth/references/api-keys.md",
      "Agents/skills/auth/references/authentication.md",
      "Agents/skills/auth/references/common-patterns.md",
      "Agents/skills/auth/references/database-tables.md",
      "Agents/skills/auth/references/decisions.md",
      "Agents/skills/auth/references/learn-log.md",
      "Agents/skills/auth/references/rbac.md",
      "Agents/skills/auth/references/rbac.md",
      "Agents/skills/auth/references/security.md",
      "Agents/skills/auth/references/troubleshooting.md",
      "Agents/skills/ci-deploy/SKILL.md",
      "Agents/skills/ci-deploy/references/advanced-pipelines.md",
      "Agents/skills/ci-deploy/references/decisions.md",
      "Agents/skills/ci-deploy/references/docker.md",
      "Agents/skills/ci-deploy/references/gcp.md",
      "Agents/skills/ci-deploy/references/kubernetes.md",
      "Agents/skills/ci-deploy/references/learn-log.md",
      "Agents/skills/ci-deploy/references/pipelines.md",
      "Agents/skills/ci-deploy/references/secrets.md",
      "Agents/skills/ci-deploy/references/secrets.md",
      "Agents/skills/database/references/migrations.md",
      "Agents/skills/integrations/references/oauth-flows.md",
      "Agents/tasks",
      "Agents/tasks/2025-01-13-integrations-onboarding-oauth.md",
      "Agents/tasks/celery-cloudbuild-deploy.md",
      "Agents/tasks/celery-redis-secret-wiring.md",
      "Agents/tasks/dedicated-repo-migration.md",
      "Agents/tasks/fix-bootstrap-permission-case.md",
      "Agents/tasks/fix-environment-discovery-migration.md",
      "Agents/tasks/fix-mordor-roles-permissions-404.md",
      "Agents/tasks/fix-preauth-error-production.md",
      "Agents/tasks/google-oauth-onboarding.md",
      "Agents/tasks/merge-environment-0036-migrations.md",
      "Agents/tasks/otel-step1-deployment.md",
      "Agents/tasks/rbac-implementation-plan-intake.md",
      "Agents/tasks/rbac-pr5-pr8.md",
      "Agents/tasks/rbac-role-management-cleanup.md",
      "Agents/tasks/rbac-role-management-permissions.md",
      "Agents/tasks/role-management-permissions-check.md",
      "TODO",
      "alerts",
      "apps",
      "apps/customer",
      "apps/customer/src/entry-authenticated.tsx",
      "apps/mordor",
      "apps/mordor/src/entry-authenticated.tsx",
      "apps/organizations",
      "apps/organizations/src/entry-authenticated.tsx",
      "backend",
      "backend/MIGRATION_SCRIPT.py",
      "backend/accounts",
      "backend/accounts/admin_rbac_api_views.py",
      "backend/accounts/admin_rbac_views.py",
      "backend/accounts/auth0_management.py",
      "backend/accounts/auth_analytics_models.py",
      "backend/accounts/auth_analytics_serializers.py",
      "backend/accounts/auth_analytics_views.py",
      "backend/accounts/management/commands/rbac_dump_casl_catalog.py",
      "backend/accounts/management/commands/rbac_lifecycle_tick.py",
      "backend/accounts/management/commands/rbac_roles_summary.py",
      "backend/accounts/management/commands/rbac_seed_permissions.py",
      "backend/accounts/middleware_auth_enforcement.py",
      "backend/accounts/middleware_rbac_identity.py",
      "backend/accounts/migrations/0001_initial.py",
      "backend/accounts/migrations/0002_organization.py",
      "backend/accounts/migrations/0003_userprofile_org_default.py",
      "backend/accounts/migrations/0004_rls_userprofile.py",
      "backend/accounts/migrations/0005_tenant_membership.py",
      "backend/accounts/migrations/0006_userprofile_tenant_nullable.py",
      "backend/accounts/migrations/0007_seed_default_tenants_assign.py",
      "backend/accounts/migrations/0008_userprofile_tenant_nonnull.py",
      "backend/accounts/migrations/0009_rls_userprofile_tenant_update.py",
      "backend/accounts/migrations/0010_alter_userprofile_organization_and_more.py",
      "backend/accounts/migrations/0011_profile_identity_fields.py",
      "backend/accounts/migrations/0012_profile_phone_split.py",
      "backend/accounts/migrations/0013_team_and_identity_extras.py",
      "backend/accounts/migrations/0014_team_id_default.py",
      "backend/accounts/migrations/0015_userprofile_notification_prefs.py",
      "backend/accounts/migrations/0016_userprofile_tz_locale_notif_state.py",
      "backend/accounts/migrations/0017_tenant_notification_policy.py",
      "backend/accounts/migrations/0018_tenant_lifecycle_and_admin_models.py",
      "backend/accounts/migrations/0019_plan_entitlements.py",
      "backend/accounts/migrations/0020_alter_plandefinition_id_and_more.py",
      "backend/accounts/migrations/0021_internal_scopes_and_profile_flag.py",
      "backend/accounts/migrations/0022_custom_attributes.py",
      "backend/accounts/migrations/0023_team_user_custom.py",
      "backend/accounts/migrations/0024_rbac_registry.py",
      "backend/accounts/migrations/0024_rbac_registry.py",
      "backend/accounts/migrations/0025_role_archive.py",
      "backend/accounts/migrations/0025_search_trgm_indexes.py",
      "backend/accounts/migrations/0026_alter_customattributedefinition_id.py",
      "backend/accounts/migrations/0027_merge_20250922_0837.py",
      "backend/accounts/migrations/0028_permission_meta.py",
      "backend/accounts/migrations/0028_permission_meta.py",
      "backend/accounts/migrations/0028_role_risk_fields.py",
      "backend/accounts/migrations/0029_permission_metadata.py",
      "backend/accounts/migrations/0029_permission_metadata.py",
      "backend/accounts/migrations/0030_userprofile_ui_prefs.py",
      "backend/accounts/migrations/0031_enable_tenant_rls.py",
      "backend/accounts/migrations/0032_organization_hierarchy.py",
      "backend/accounts/migrations/0033_remove_organization_org_parent_idx_and_more.py",
      "backend/accounts/migrations/0034_check_constraints.py",
      "backend/accounts/migrations/0035_organization_profile_fields.py",
      "backend/accounts/migrations/0036_grc_organization_fields.py",
      "backend/accounts/migrations/0037_remove_sso_mfa_fields.py",
      "backend/accounts/migrations/0038_alter_organization_tax_id.py",
      "backend/accounts/migrations/0039_tenant_api_calls_month_tenant_api_calls_today_and_more.py",
      "backend/accounts/migrations/0040_tenant_admin_notification_message_and_more.py",
      "backend/accounts/migrations/0041_rolev2_organization_parent_userprofile_primary_team_and_more.py",
      "backend/accounts/migrations/0042_tenanthealthalertrule_tenanthealthmetric_and_more.py",
      "backend/accounts/migrations/0043_broadcasttemplate_scheduledbroadcast_and_more.py",
      "backend/accounts/migrations/0044_alter_scopedpermission_resource_roletemplate.py",
      "backend/accounts/migrations/0044_alter_scopedpermission_resource_roletemplate.py",
      "backend/accounts/migrations/0045_rolev2_tags.py",
      "backend/accounts/migrations/0046_remove_business_unit_and_update_team_types.py",
      "backend/accounts/migrations/0047_sync_rbac_permissions.py",
      "backend/accounts/migrations/0047_sync_rbac_permissions.py",
      "backend/accounts/migrations/0048_remove_userprofile_role.py",
      "backend/accounts/migrations/0049_alter_scopedpermission_resource.py",
      "backend/accounts/migrations/0049_alter_scopedpermission_resource.py",
      "backend/accounts/migrations/0999_rename_rolev2_to_role.py",
      "backend/accounts/migrations/1000_alter_role_options_alter_role_tenant.py",
      "backend/accounts/migrations/1001_drop_legacy_rbac_tables.py",
      "backend/accounts/migrations/1001_drop_legacy_rbac_tables.py",
      "backend/accounts/migrations/1002_recreate_role_permissions_through_table.py",
      "backend/accounts/migrations/1002_recreate_role_permissions_through_table.py",
      "backend/accounts/migrations/1003_alter_scopedpermission_resource.py",
      "backend/accounts/migrations/1003_alter_scopedpermission_resource.py",
      "backend/accounts/migrations/1004_alter_scopedpermission_action.py",
      "backend/accounts/migrations/1004_alter_scopedpermission_action.py",
      "backend/accounts/migrations/1005_add_device_and_session_models.py",
      "backend/accounts/migrations/1006_alter_scopedpermission_resource.py",
      "backend/accounts/migrations/1006_alter_scopedpermission_resource.py",
      "backend/accounts/migrations/1007_add_dashboard_resource.py",
      "backend/accounts/migrations/1008_entitlements_catalog.py",
      "backend/accounts/migrations/1009_seed_owner_internal.py",
      "backend/accounts/migrations/1010_subscription_split.py",
      "backend/accounts/migrations/1011_alter_catalogsubscription_id_alter_creditgrant_id_and_more.py",
      "backend/accounts/migrations/1012_merge_20251105_2056.py",
      "backend/accounts/migrations/1013_delete_rolev2_remove_role_archived_and_more.py",
      "backend/accounts/migrations/1014_notification_columns_and_locale_fields.py",
      "backend/accounts/migrations/1015_merge_20251122_2008.py",
      "backend/accounts/migrations/1016_add_account_models.py",
      "backend/accounts/migrations/1017_assign_demo_admin.py",
      "backend/accounts/migrations/1018_remove_demo_fullaccess_prod.py",
      "backend/accounts/migrations/1019_add_integrations_permissions.py",
      "backend/accounts/migrations/1019_add_integrations_permissions.py",
      "backend/accounts/migrations/1020_add_user_search_trgm_indexes.py",
      "backend/accounts/migrations/1021_role_risk_level_role_risk_meta_and_more.py",
      "backend/accounts/migrations/1022_userprofile_rls_by_user_id.py",
      "backend/accounts/migrations/1023_standardize_rls_gucs.py",
      "backend/accounts/migrations/1024_account_assetentity_fk.py",
      "backend/accounts/migrations/1025_account_roles_permissions_auth.py",
      "backend/accounts/migrations/1025_account_roles_permissions_auth.py",
      "backend/accounts/migrations/1025_account_roles_permissions_auth.py",
      "backend/accounts/migrations/1026_remove_account_accounts_ac_tenant__auth_idx_and_more.py",
      "backend/accounts/migrations/1026_remove_account_accounts_ac_tenant__auth_idx_and_more.py",
      "backend/accounts/migrations/1027_add_risk_rules_permissions.py",
      "backend/accounts/migrations/1027_add_risk_rules_permissions.py",
      "backend/accounts/migrations/1028_add_account_risk_fields.py",
      "backend/accounts/migrations/1029_add_finding_template_model.py",
      "backend/accounts/migrations/1030_role_is_template_role_source_template_and_more.py",
      "backend/accounts/migrations/1031_role_templates_global.py",
      "backend/accounts/migrations/1032_remove_role_accounts_role_template_requires_null_tenant_and_more.py",
      "backend/accounts/migrations/1033_drop_scopedpermission_and_legacy_role_fields.py",
      "backend/accounts/migrations/1033_drop_scopedpermission_and_legacy_role_fields.py",
      "backend/accounts/migrations/1034_alter_userroleassignment_scope_type_and_more.py",
      "backend/accounts/migrations/1035_roleriskpolicy.py",
      "backend/accounts/migrations/1036_account_oauth_scopes.py",
      "backend/accounts/migrations/1036_account_oauth_scopes.py",
      "backend/accounts/migrations/1037_add_external_avatar_url.py",
      "backend/accounts/migrations/1038_grant_demo_admin_v3.py",
      "backend/accounts/migrations/1039_rbac_homogenization.py",
      "backend/accounts/migrations/1039_rbac_homogenization.py",
      "backend/accounts/migrations/1040_remove_permissionauditlog_actor_and_more.py",
      "backend/accounts/migrations/1040_remove_permissionauditlog_actor_and_more.py",
      "backend/accounts/migrations/1041_alter_role_permissions_v3.py",
      "backend/accounts/migrations/1041_alter_role_permissions_v3.py",
      "backend/accounts/migrations/1042_migrate_permissions_to_canonical.py",
      "backend/accounts/migrations/1042_migrate_permissions_to_canonical.py",
      "backend/accounts/migrations/1043_access_grants_and_scope_types.py",
      "backend/accounts/migrations/1044_tenant_slug_global_unique.py",
      "backend/accounts/migrations/1045_rename_accounts_acc_grantor_status_idx_accounts_ac_grantor_970445_idx_and_more.py",
      "backend/accounts/migrations/1046_tenant_onboarding_apps_score_and_more.py",
      "backend/accounts/migrations/1047_tenant_dns_discovery_seed_fields.py",
      "backend/accounts/migrations/1048_authanalyticssummary_authevent.py",
      "backend/accounts/migrations/1048_authanalyticssummary_authevent.py",
      "backend/accounts/migrations/1048_seed_free_plan.py",
      "backend/accounts/migrations/1049_add_domain_role_exposure.py",
      "backend/accounts/migrations/1050_change_domain_role_to_roles_array.py",
      "backend/accounts/migrations/1051_tenant_profiles.py",
      "backend/accounts/migrations/1052_seed_tenant_profiles.py",
      "backend/accounts/migrations/1053_tenant_profile_templates.py",
      "backend/accounts/migrations/1054_seed_tenant_profile_templates.py",
      "backend/accounts/migrations/1055_role_templates_scope_and_profiles.py",
      "backend/accounts/migrations/1056_alter_role_organization_and_more.py",
      "backend/accounts/migrations/1057_tenantdomain_asset_entity.py",
      "backend/accounts/migrations/1058_role_template_visibility_and_auto_create.py",
      "backend/accounts/migrations/1059_fix_account_asset_fk_constraint.py",
      "backend/accounts/migrations/1060_enforce_userprofile_rls.py",
      "backend/accounts/migrations/1061_external_groups.py",
      "backend/accounts/migrations/1062_rename_accounts_ex_tenant__3a632a_idx_accounts_ex_tenant__0c1f4d_idx_and_more.py",
      "backend/accounts/migrations/1063_role_is_platform_staff.py",
      "backend/accounts/migrations/1064_platform_roles.py",
      "backend/accounts/migrations/1065_usersession_realm_enforcement.py",
      "backend/accounts/migrations/1066_remove_platformroleassignment_platform_role_assignment_user_role_uniq_and_more.py",
      "backend/accounts/migrations/1067_consolidate_data_models.py",
      "backend/accounts/migrations/1068_alter_organization_options_alter_team_options_and_more.py",
      "backend/accounts/migrations/1069_documentslot_and_status.py",
      "backend/accounts/migrations/1070_platform_role_assignment_starts_at.py",
      "backend/accounts/migrations/1071_merge_20260202_1350.py",
      "backend/accounts/migrations/1072_seed_default_platform_roles.py",
      "backend/accounts/migrations/1073_feature_key_allow_dots.py",
      "backend/accounts/migrations/1074_aeptus_support_access.py",
      "backend/accounts/migrations/1075_alter_usertenantmembership_role.py",
      "backend/accounts/migrations/1076_userprofile_rls_insert_policy.py",
      "backend/accounts/migrations/1077_migrate_platform_roles_to_canonical_rbac.py",
      "backend/accounts/migrations/1077_migrate_platform_roles_to_canonical_rbac.py",
      "backend/accounts/migrations/1078_userprofile_rls_include_memberships.py",
      "backend/accounts/migrations/1079_userprofile_archived_at.py",
      "backend/accounts/migrations/1080_profile_integrity_jobs.py",
      "backend/accounts/migrations/1081_impersonation_ticket_and_request_id.py",
      "backend/accounts/migrations/1082_alter_tenant_options.py",
      "backend/accounts/migrations/1083_alter_scheduledbroadcast_status.py",
      "backend/accounts/migrations/1084_tenant_profile_fk_and_framework_template.py",
      "backend/accounts/migrations/1085_seed_baseline_framework_templates.py",
      "backend/accounts/migrations/1086_merge_20260305_1932.py",
      "backend/accounts/migrations/FOLDER.migrations.md",
      "backend/accounts/migrations/__init__.py",
      "backend/accounts/permissions_base.py",
      "backend/accounts/rbac.py",
      "backend/accounts/rbac_audit_models.py",
      "backend/accounts/rbac_canonical.py",
      "backend/accounts/rbac_helpers.py",
      "backend/accounts/rbac_models.py",
      "backend/accounts/rbac_permissions.py",
      "backend/accounts/rbac_scope.py",
      "backend/accounts/rbac_signals.py",
      "backend/accounts/tests/test_rbac_access_engine.py",
      "backend/accounts/tests/test_rbac_lifecycle_tick.py",
      "backend/accounts/tests/test_rbac_on_behalf_audit.py",
      "backend/accounts/tests/test_rbac_team_auto_assign.py",
      "backend/adn",
      "backend/adn/migrations/0001_initial.py",
      "backend/adn/migrations/0002_enable_rls.py",
      "backend/adn/migrations/0003_fix_category_slug_uniqueness.py",
      "backend/adn/migrations/0004_pipelinerun_enrichmentqueue_directorysignal_and_more.py",
      "backend/adn/migrations/0005_localproviderentry_localserviceentry_and_more.py",
      "backend/adn/migrations/0006_remove_localproviderentry_unique_local_provider_domain_per_tenant_and_more.py",
      "backend/adn/migrations/0007_add_schema_version.py",
      "backend/adn/migrations/0008_directorycategory_expected_at_onboarding.py",
      "backend/adn/migrations/0009_add_app_metadata_facts.py",
      "backend/adn/migrations/0010_expand_fact_types.py",
      "backend/adn/migrations/0011_add_category_owner_fields.py",
      "backend/adn/migrations/0012_pipelinerun_add_adn_onboarding_enrich_stage.py",
      "backend/adn/migrations/0013_category_owner_delegation.py",
      "backend/adn/migrations/0014_pipelinestageconfig.py",
      "backend/adn/migrations/0015_remove_directoryfact_fact_single_target_entity_and_more.py",
      "backend/adn/migrations/0016_sitemap_supply_chain_choice_expansions.py",
      "backend/adn/migrations/0017_rename_enrichmentqueue_pipelinequeue.py",
      "backend/adn/migrations/0018_rename_adn_pipelin_target__70e8a1_idx_adn_pipelin_target__d13f85_idx_and_more.py",
      "backend/adn/migrations/0019_pipelinebatch.py",
      "backend/adn/migrations/0020_rename_adn_pipelin_status_batch_idx_adn_pipelin_status_90c11e_idx_and_more.py",
      "backend/adn/migrations/__init__.py",
      "backend/adn/permissions.py",
      "backend/adn/tests/test_permissions.py",
      "backend/aep_backend",
      "backend/ai_providers",
      "backend/ai_providers/migrations/0001_initial.py",
      "backend/ai_providers/migrations/0002_seed_providers.py",
      "backend/ai_providers/migrations/__init__.py",
      "backend/analytics",
      "backend/analytics/migrations/0001_initial.py",
      "backend/analytics/migrations/__init__.py",
      "backend/api_keys",
      "backend/api_keys/migrations/0001_initial.py",
      "backend/api_keys/migrations/0002_rename_api_keys_tenant__a3f8b1_idx_api_keys_tenant__aa40c3_idx_and_more.py",
      "backend/api_keys/migrations/0003_unique_constraints.py",
      "backend/api_keys/migrations/0004_apikey_user.py",
      "backend/api_keys/migrations/__init__.py",
      "backend/api_usage",
      "backend/api_usage/migrations/0001_initial.py",
      "backend/api_usage/migrations/0002_enable_rls.py",
      "backend/api_usage/migrations/0003_rename_api_deprec_tenant_status_idx_api_depreca_tenant__60e9d0_idx_and_more.py",
      "backend/api_usage/migrations/0004_brin_indexes.py",
      "backend/api_usage/migrations/0005_merge_20251001_1316.py",
      "backend/api_usage/migrations/0006_standardize_rls_gucs.py",
      "backend/api_usage/migrations/__init__.py",
      "backend/audit",
      "backend/audit/migrations/0001_initial.py",
      "backend/audit/migrations/0002_rls_and_brin.py",
      "backend/audit/migrations/0003_dedup_unique.py",
      "backend/audit/migrations/0003_partition_shadow_table.py",
      "backend/audit/migrations/0004_alter_auditeventv2_options_and_more.py",
      "backend/audit/migrations/0004_audit_export_job.py",
      "backend/audit/migrations/0005_audit_ingest_keys.py",
      "backend/audit/migrations/0005_audit_phase3_enhancements.py",
      "backend/audit/migrations/0006_auditpolicy_legal_hold.py",
      "backend/audit/migrations/0007_auditpolicy_retention_status.py",
      "backend/audit/migrations/0008_auditeventv2_perf_indexes.py",
      "backend/audit/migrations/0009_merge_0004_0008.py",
      "backend/audit/migrations/0010_drop_audit_event_legacy.py",
      "backend/audit/migrations/0011_alter_auditexportjob_id.py",
      "backend/audit/migrations/0012_auditexportjob_expires_at_auditexportjob_format_and_more.py",
      "backend/audit/migrations/0013_standardize_rls_gucs.py",
      "backend/audit/migrations/0014_actor_id_string.py",
      "backend/audit/migrations/FOLDER.migrations.md",
      "backend/audit/migrations/__init__.py",
      "backend/automations",
      "backend/automations/migrations/0001_initial.py",
      "backend/automations/migrations/0002_definition.py",
      "backend/automations/migrations/0003_data_model_rest.py",
      "backend/automations/migrations/0004_run_logs.py",
      "backend/automations/migrations/0005_enable_rls.py",
      "backend/automations/migrations/0006_check_constraints.py",
      "backend/automations/migrations/0007_performance_indexes.py",
      "backend/automations/migrations/0008_brin_indexes.py",
      "backend/automations/migrations/0009_partial_indexes.py",
      "backend/automations/migrations/0010_remove_automationdefinition_auto_def_tenant_status_idx_and_more.py",
      "backend/automations/migrations/0011_merge_20251106_2056.py",
      "backend/automations/migrations/0012_remove_automationdefinition_created_by_and_more.py",
      "backend/automations/migrations/0013_standardize_rls_gucs.py",
      "backend/automations/migrations/0014_eventdeadletter_payload_json.py",
      "backend/automations/migrations/__init__.py",
      "backend/collaboration",
      "backend/collaboration/migrations/0001_initial.py",
      "backend/collaboration/migrations/0002_review_models.py",
      "backend/collaboration/migrations/0003_rename_collaborati_locatio_idx_collaborati_locatio_8dcb41_idx_and_more.py",
      "backend/collaboration/migrations/__init__.py",
      "backend/common",
      "backend/community",
      "backend/community/migrations/0001_initial.py",
      "backend/community/migrations/0002_enable_rls_policies.py",
      "backend/community/migrations/0003_alter_implicitsignal_target_type.py",
      "backend/community/migrations/__init__.py",
      "backend/controls",
      "backend/controls/migrations/0001_initial.py",
      "backend/controls/migrations/0002_performance_indexes.py",
      "backend/controls/migrations/0003_custom_dashboards.py",
      "backend/controls/migrations/0004_remove_controldefinition_ctrl_tenant_status_domain_idx_and_more.py",
      "backend/controls/migrations/0005_add_control_assessment_items.py",
      "backend/controls/migrations/0006_add_scope_dsl_fields.py",
      "backend/controls/migrations/0007_rename_ctrl_item_tenant_occ_idx_controls_co_tenant__9e7728_idx_and_more.py",
      "backend/controls/migrations/0008_access_review_fields.py",
      "backend/controls/migrations/0009_item_validity_fields.py",
      "backend/controls/migrations/0009_rename_controls_co_tenant_c_kind_idx_controls_co_tenant__9e629c_idx_and_more.py",
      "backend/controls/migrations/0010_merge_20251007_1220.py",
      "backend/controls/migrations/0010_occurrence_signoff_fields.py",
      "backend/controls/migrations/0011_merge_20251007_1252.py",
      "backend/controls/migrations/0012_rename_controls_occ_signoff_due_idx_controls_co_signoff_7bb034_idx.py",
      "backend/controls/migrations/0013_controldefinition_business_unit_and_more.py",
      "backend/controls/migrations/0014_add_composite_indexes.py",
      "backend/controls/migrations/0014_add_performance_indexes.py",
      "backend/controls/migrations/0015_merge_20251104_0914.py",
      "backend/controls/migrations/0016_evidence_and_more.py",
      "backend/controls/migrations/0017_add_search_vector_control.py",
      "backend/controls/migrations/0018_controldefinition_idx_control_search.py",
      "backend/controls/migrations/0019_controldefinition_idx_control_search.py",
      "backend/controls/migrations/0020_enable_rls.py",
      "backend/controls/migrations/0021_evidence_artifact_scan_fields.py",
      "backend/controls/migrations/0022_framework_requirement_and_policy_mapping.py",
      "backend/controls/migrations/0023_populate_framework_requirements.py",
      "backend/controls/migrations/0024_rename_controls_fr_framewo_idx_cat_controls_fr_framewo_83042f_idx_and_more.py",
      "backend/controls/migrations/__init__.py",
      "backend/controls/permissions.py",
      "backend/controls/tests/test_permissions.py",
      "backend/controls/tests/test_rbac_boundary.py",
      "backend/core",
      "backend/core/management/commands/create_search_permissions.py",
      "backend/core/migrations/0001_initial.py",
      "backend/core/migrations/0003_inapp_security_evidence.py",
      "backend/core/migrations/0004_alerts_evidence_meta_url.py",
      "backend/core/migrations/0005_auditevent_healthcheck_and_more.py",
      "backend/core/migrations/0006_outbound_email_job.py",
      "backend/core/migrations/0007_emailjob_partial_idx.py",
      "backend/core/migrations/0008_outbound_email_bodyhash_unique.py",
      "backend/core/migrations/0009_remove_outboundemailjob_core_emailjob_triplet_uniq_and_more.py",
      "backend/core/migrations/0010_pg_stat_statements_extension.py",
      "backend/core/migrations/0011_delete_auditevent.py",
      "backend/core/migrations/0012_drop_core_auditevent.py",
      "backend/core/migrations/0013_rlsauditevent.py",
      "backend/core/migrations/0014_designsystempage_designsystemcomponent_and_more.py",
      "backend/core/migrations/0015_add_planned_components.py",
      "backend/core/migrations/0016_add_resource_permission_models.py",
      "backend/core/migrations/0016_add_resource_permission_models.py",
      "backend/core/migrations/0017_rlsauditevent.py",
      "backend/core/migrations/0018_change_default_visibility_to_tenant.py",
      "backend/core/migrations/0019_tenantattribute_moduleattributeconfig_and_more.py",
      "backend/core/migrations/0020_queryperformancelog_queryperformancestats.py",
      "backend/core/migrations/0021_rename_core_queryp_created_af2bd6_idx_core_queryp_created_ff0917_idx_and_more.py",
      "backend/core/migrations/0022_merge_20251106_2056.py",
      "backend/core/migrations/0023_search_analytics_models.py",
      "backend/core/migrations/0024_alter_searchanalytics_created_by_and_more.py",
      "backend/core/migrations/0025_export_job.py",
      "backend/core/migrations/0026_enable_rls_core_export_job.py",
      "backend/core/migrations/0027_alter_exportjob_format_alter_exportjob_status.py",
      "backend/core/migrations/FOLDER.migrations.md",
      "backend/core/migrations/__init__.py",
      "backend/core/permissions.py",
      "backend/core/permissions/__init__.py",
      "backend/core/permissions/decorators.py",
      "backend/core/permissions/helpers.py",
      "backend/core/permissions/policy.py",
      "backend/core/permissions/rbac.py",
      "backend/core/permissions/rls_queryset_manager.py",
      "backend/core/permissions/test_utils.py",
      "backend/core/permissions/tests/__init__.py",
      "backend/core/permissions/tests/test_decorators.py",
      "backend/core/permissions/tests/test_helpers.py",
      "backend/core/permissions/tests/test_policy.py",
      "backend/core/tests/test_search/test_search_permissions.py",
      "backend/deployment-guide.md",
      "backend/directory",
      "backend/directory/migrations/0001_initial.py",
      "backend/directory/migrations/0002_bitemporal_constraints.py",
      "backend/directory/migrations/0003_add_service_offering_technology.py",
      "backend/directory/migrations/0004_bitemporal_exclusion_constraints.py",
      "backend/directory/migrations/0005_check_constraints.py",
      "backend/directory/migrations/0006_alter_technologycomponent_unique_together_and_more.py",
      "backend/directory/migrations/0007_service_offering_technology_constraints_and_cleanup.py",
      "backend/directory/migrations/0008_legalentity_categories_serviceoffering_categories_and_more.py",
      "backend/directory/migrations/0009_legalentity_serviceoffering_expansion.py",
      "backend/directory/migrations/0010_legalentity_industry_sanctions_jurisdiction.py",
      "backend/directory/migrations/0011_allow_null_legal_entity_on_service_offering.py",
      "backend/directory/migrations/0012_remove_technologycomponent_categories_and_more.py",
      "backend/directory/migrations/0013_fix_techcat_null_distinct.py",
      "backend/directory/migrations/0014_remove_serviceoffering_tags.py",
      "backend/directory/migrations/0015_technologyproduct_categories.py",
      "backend/directory/migrations/0016_technologycategory_is_active.py",
      "backend/directory/migrations/__init__.py",
      "backend/docs",
      "backend/documents",
      "backend/documents/deployment-guide.md",
      "backend/documents/migrations/0001_initial.py",
      "backend/documents/migrations/0002_rename_doc_t_type_deleted_idx_documents_d_tenant__5ef7dd_idx_and_more.py",
      "backend/documents/migrations/0003_enable_rls.py",
      "backend/documents/migrations/0004_expand_doctype_and_relations.py",
      "backend/documents/migrations/0005_documentslot_and_status.py",
      "backend/documents/migrations/0006_documenttypeprofile.py",
      "backend/documents/migrations/__init__.py",
      "backend/environment",
      "backend/environment/migrations/0001_initial.py",
      "backend/environment/migrations/0002_add_owned_resource_mixin.py",
      "backend/environment/migrations/0002_initial.py",
      "backend/environment/migrations/0002_riskrule_riskrulefielddefinition_riskruleexecution.py",
      "backend/environment/migrations/0003_add_business_security_ownership.py",
      "backend/environment/migrations/0004_alter_asset_visibility_and_more.py",
      "backend/environment/migrations/0005_remove_asset_criticality_asset_service_asset_tier.py",
      "backend/environment/migrations/0006_add_composite_indexes.py",
      "backend/environment/migrations/0006_add_performance_indexes.py",
      "backend/environment/migrations/0007_merge_20251104_0914.py",
      "backend/environment/migrations/0008_remove_asset_env_asset_lifecycle_risk_idx_and_more.py",
      "backend/environment/migrations/0009_merge_20251106_2056.py",
      "backend/environment/migrations/0010_remove_asset_environment_owner_t_925258_idx_and_more.py",
      "backend/environment/migrations/0011_asset_idx_asset_type_tier_stat_and_more.py",
      "backend/environment/migrations/0012_add_search_vector_asset.py",
      "backend/environment/migrations/0013_asset_idx_asset_search.py",
      "backend/environment/migrations/0014_asset_managed_by_thirdpartyentity.py",
      "backend/environment/migrations/0015_alter_asset_unique_together_and_more.py",
      "backend/environment/migrations/0017_bitemporal_table_maintenance_tuning.py",
      "backend/environment/migrations/0018_asset_constraints_and_assettechnology_pair_unique.py",
      "backend/environment/migrations/0019_enable_rls.py",
      "backend/environment/migrations/0020_standardize_risk_fields.py",
      "backend/environment/migrations/0021_merge_20251216_1225.py",
      "backend/environment/migrations/0022_rename_env_riskrule_tenant_target_idx_environment_tenant__8ca752_idx_and_more.py",
      "backend/environment/migrations/0023_riskrulefielddefinition_category.py",
      "backend/environment/migrations/0024_add_asset_risk_breakdown.py",
      "backend/environment/migrations/0025_sprint14_asset_enhancements.py",
      "backend/environment/migrations/0026_rename_env_compmap_t_stat_idx_environment_tenant__24f617_idx_and_more.py",
      "backend/environment/migrations/0026_update_asset_search_vector_business_unit_option.py",
      "backend/environment/migrations/0027_merge_20251224_0905.py",
      "backend/environment/migrations/0028_asset_hosting_model_asset_local_service_and_more.py",
      "backend/environment/migrations/0029_asset_data_model_v12_1.py",
      "backend/environment/migrations/0030_remove_asset_idx_asset_category_and_more.py",
      "backend/environment/migrations/0031_risk_rule_library.py",
      "backend/environment/migrations/0032_remove_orgrulevisibility_unique_org_rule_visibility_and_more.py",
      "backend/environment/migrations/0033_add_asset_domain_registration_fields.py",
      "backend/environment/migrations/0034_alter_asset_asset_type.py",
      "backend/environment/migrations/0035_domain_analyzer_integration_sprint16.py",
      "backend/environment/migrations/0036_asset_discovery_tracking_fields.py",
      "backend/environment/migrations/0036_rename_idx_certhistory_asset_environment_tenant__3daa83_idx_and_more.py",
      "backend/environment/migrations/0037_merge_20260109_0700.py",
      "backend/environment/migrations/0038_threat_intelligence_traffic_ranking.py",
      "backend/environment/migrations/0039_remove_asset_idx_asset_threat_malicious_and_more.py",
      "backend/environment/migrations/0040_technology_fingerprinting.py",
      "backend/environment/migrations/0041_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
      "backend/environment/migrations/0042_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
      "backend/environment/migrations/0043_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
      "backend/environment/migrations/0044_asset_discovery_sources.py",
      "backend/environment/migrations/0045_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
      "backend/environment/migrations/0046_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
      "backend/environment/migrations/0047_update_threatintelligencecheck_constraint.py",
      "backend/environment/migrations/0048_backfill_asset_discovery_sources.py",
      "backend/environment/migrations/0049_asset_directory_category.py",
      "backend/environment/migrations/0050_remove_technologycategory_parent_and_more.py",
      "backend/environment/migrations/0051_remove_asset_business_function_id_and_more.py",
      "backend/environment/migrations/__init__.py",
      "backend/environment/permissions.py",
      "backend/environment/risk_rule_library_permissions.py",
      "backend/environment/risk_rule_permissions.py",
      "backend/events",
      "backend/events/migrations/0001_initial.py",
      "backend/events/migrations/0002_add_owned_resource_mixin.py",
      "backend/events/migrations/0003_add_business_security_ownership.py",
      "backend/events/migrations/0004_alter_event_visibility_and_more.py",
      "backend/events/migrations/0005_incident.py",
      "backend/events/migrations/0006_delete_incident.py",
      "backend/events/migrations/0007_incident_assetentity.py",
      "backend/events/migrations/0008_alter_incident_created_at_alter_incident_created_by_and_more.py",
      "backend/events/migrations/0009_alter_incident_business_owner_team_and_more.py",
      "backend/events/migrations/__init__.py",
      "backend/frameworks",
      "backend/frameworks/migrations/__init__.py",
      "backend/gcp/deploy.sh",
      "backend/gcp/setup-search-infrastructure.sh",
      "backend/guide-migrations.md",
      "backend/information",
      "backend/information/migrations/0001_initial.py",
      "backend/information/migrations/0001_initial_ims_models.py",
      "backend/information/migrations/0002_alter_document_content_type_and_more.py",
      "backend/information/migrations/0003_merge_20251106_2056.py",
      "backend/information/migrations/0004_assetentity_fks_and_contenttypes.py",
      "backend/information/migrations/0005_alter_privacyprofile_content_type.py",
      "backend/information/migrations/__init__.py",
      "backend/information/models/migration.py",
      "backend/information/serializers/migration.py",
      "backend/integrations",
      "backend/integrations/migrations/0001_initial.py",
      "backend/integrations/migrations/0002_alter_integrationconnection_provider.py",
      "backend/integrations/migrations/0003_slackinstall.py",
      "backend/integrations/migrations/0004_rename_integrations_slack_tenant_team_idx_integration_tenant__f6ace6_idx.py",
      "backend/integrations/migrations/0005_enhance_integrationconnection_for_adapter_pattern.py",
      "backend/integrations/migrations/0006_enhance_integration_connection.py",
      "backend/integrations/migrations/0007_integrationfieldmapping.py",
      "backend/integrations/migrations/0008_scaling_architecture.py",
      "backend/integrations/migrations/0009_alter_integrationprovider_options_and_more.py",
      "backend/integrations/migrations/0010_integrationaction_integrationdatapoint.py",
      "backend/integrations/migrations/0011_seed_google_workspace_actions_complete.py",
      "backend/integrations/migrations/0012_integrationaction_category.py",
      "backend/integrations/migrations/0013_seed_google_workspace_webhooks.py",
      "backend/integrations/migrations/0014_seed_slack_provider_and_actions.py",
      "backend/integrations/migrations/0015_seed_github_provider_and_actions.py",
      "backend/integrations/migrations/0016_add_is_automation_enabled.py",
      "backend/integrations/migrations/0017_integrationaction_integration_auto_en_idx_and_more.py",
      "backend/integrations/migrations/0018_integration_sync_history.py",
      "backend/integrations/migrations/0019_add_sync_history_data_snapshots.py",
      "backend/integrations/migrations/0020_add_sync_type_choices.py",
      "backend/integrations/migrations/0021_rename_nango_connection_id.py",
      "backend/integrations/migrations/0022_normalize_integration_provider_categories.py",
      "backend/integrations/migrations/0023_seed_microsoft_365_provider.py",
      "backend/integrations/migrations/0024_seed_microsoft_teams_provider.py",
      "backend/integrations/migrations/0025_normalize_connected_status_to_active.py",
      "backend/integrations/migrations/0026_seed_google_workspace_provider.py",
      "backend/integrations/migrations/__init__.py",
      "backend/integrations/tests/test_token_lifecycle_guardrails.py",
      "backend/integrations/tests/test_token_refresh.py",
      "backend/integrations/token-lifecycle-standard.md",
      "backend/k8s",
      "backend/knowledge",
      "backend/knowledge/migrations/0001_initial.py",
      "backend/knowledge/migrations/0002_remove_controlmapping_unique_policy_requirement_and_more.py",
      "backend/knowledge/migrations/__init__.py",
      "backend/localization",
      "backend/localization/migrations/0001_initial.py",
      "backend/localization/migrations/0002_add_owned_resource_mixin.py",
      "backend/localization/migrations/0003_add_business_security_ownership.py",
      "backend/localization/migrations/0004_alter_glossaryterm_visibility_and_more.py",
      "backend/localization/migrations/0005_add_analytics_models.py",
      "backend/localization/migrations/0006_alter_translationchangelog_created_by_and_more.py",
      "backend/localization/migrations/0007_translation_ai_config.py",
      "backend/localization/migrations/__init__.py",
      "backend/manual-deploy-with-verify.sh",
      "backend/mapping_intelligence",
      "backend/mapping_intelligence/migrations/0001_initial.py",
      "backend/mapping_intelligence/migrations/0002_add_missing_fields.py",
      "backend/mapping_intelligence/migrations/0002_fielddefinition_mapping_int_synonym_49b140_gin.py",
      "backend/mapping_intelligence/migrations/0003_add_aimachinesettings.py",
      "backend/mapping_intelligence/migrations/0003_fielddefinition_tenant_scope.py",
      "backend/mapping_intelligence/migrations/0003_rename_mapping_int_entity__idx_mapping_int_entity__9ab5a0_idx_and_more.py",
      "backend/mapping_intelligence/migrations/0004_merge_20251020_1449.py",
      "backend/mapping_intelligence/migrations/0005_add_versioning_and_constraints.py",
      "backend/mapping_intelligence/migrations/0007_add_performance_indexes.py",
      "backend/mapping_intelligence/migrations/0008_merge_20251106_2056.py",
      "backend/mapping_intelligence/migrations/0009_mappinghistory_updated_at_mappinghistory_updated_by_and_more.py",
      "backend/mapping_intelligence/migrations/0010_merge_20260105_1105.py",
      "backend/mapping_intelligence/migrations/0011_remove_fielddefinition_mapping_int_entity__030d87_idx_and_more.py",
      "backend/mapping_intelligence/migrations/__init__.py",
      "backend/mapping_intelligence/permissions.py",
      "backend/menu_overrides",
      "backend/menu_overrides/migrations/0001_initial.py",
      "backend/menu_overrides/migrations/0002_add_navigation_analytics.py",
      "backend/menu_overrides/migrations/__init__.py",
      "backend/middleware",
      "backend/middleware/rbac_enforcement.py",
      "backend/onboarding",
      "backend/onboarding/migrations/0001_initial.py",
      "backend/onboarding/migrations/0002_onboardingruntimestate.py",
      "backend/onboarding/migrations/__init__.py",
      "backend/operational",
      "backend/operational/migrations/0001_initial.py",
      "backend/operational/migrations/0002_event_sourcing_triggers.py",
      "backend/operational/migrations/0003_fix_event_sourcing_trigger.py",
      "backend/operational/migrations/0004_trigger_request_id.py",
      "backend/operational/migrations/0005_trigger_request_id_metadata.py",
      "backend/operational/migrations/0006_trigger_update_merge_guard.py",
      "backend/operational/migrations/0007_trigger_merge_guard_jsonb.py",
      "backend/operational/migrations/__init__.py",
      "backend/ops/scripts/deploy-celery-jobs.sh",
      "backend/ops/scripts/deploy-rbac-seed.sh",
      "backend/ops/scripts/deploy-rbac-seed.sh",
      "backend/ops/scripts/execute-rbac-seed.sh",
      "backend/ops/scripts/run-migrations.sh",
      "backend/ops/scripts/seed-rbac-permissions.sh",
      "backend/page_actions",
      "backend/page_actions/migrations/0001_initial.py",
      "backend/page_actions/migrations/0002_remove_customaction_unique_custom_action_per_org_page_and_more.py",
      "backend/page_actions/migrations/0003_standardize_rls_gucs.py",
      "backend/page_actions/migrations/__init__.py",
      "backend/page_actions/permissions.py",
      "backend/page_actions/services/permission_service.py",
      "backend/page_actions/tests/test_permission_service.py",
      "backend/posture",
      "backend/posture/finding_template_library_permissions.py",
      "backend/posture/migrations/0001_initial.py",
      "backend/posture/migrations/0002_add_owned_resource_mixin.py",
      "backend/posture/migrations/0003_add_business_security_ownership.py",
      "backend/posture/migrations/0004_alter_campaign_visibility_alter_finding_visibility_and_more.py",
      "backend/posture/migrations/0005_add_search_vector_finding.py",
      "backend/posture/migrations/0006_finding_idx_finding_search.py",
      "backend/posture/migrations/0007_alter_campaign_scope_assets_alter_finding_asset_and_more.py",
      "backend/posture/migrations/0008_add_finding_likelihood_and_targets.py",
      "backend/posture/migrations/0009_add_finding_template_model.py",
      "backend/posture/migrations/0010_finding_template_library.py",
      "backend/posture/migrations/0011_seed_finding_template_library.py",
      "backend/posture/migrations/0012_rename_posture_ftl_category_status_idx_posture_ftl_cat_status_idx_and_more.py",
      "backend/posture/migrations/__init__.py",
      "backend/project",
      "backend/reports",
      "backend/run_migrations.py",
      "backend/scripts",
      "backend/scripts/audit_rbac_migration.py",
      "backend/scripts/audit_rbac_migration.py",
      "backend/scripts/debug/test_automations_permission_debug.py",
      "backend/scripts/debug/test_rbac_migration.py",
      "backend/scripts/debug/test_rbac_migration.py",
      "backend/setup-auto-deploy.sh",
      "backend/tasks",
      "backend/tasks/migrations/0001_initial.py",
      "backend/tasks/migrations/0002_tasklink.py",
      "backend/tasks/migrations/0003_tasksavedview.py",
      "backend/tasks/migrations/0004_task_tags.py",
      "backend/tasks/migrations/0005_task_comments_watchers.py",
      "backend/tasks/migrations/0006_task_attachment.py",
      "backend/tasks/migrations/0007_checklist_item.py",
      "backend/tasks/migrations/0008_alter_checklistitem_created_at_and_more.py",
      "backend/tasks/migrations/0008_task_workflow_sla.py",
      "backend/tasks/migrations/0009_task_provenance.py",
      "backend/tasks/migrations/0010_merge_20251006_1220.py",
      "backend/tasks/migrations/0011_add_owned_resource_mixin.py",
      "backend/tasks/migrations/0016_task_completion_rule.py",
      "backend/tasks/migrations/0017_add_business_security_ownership.py",
      "backend/tasks/migrations/0018_alter_checklistitem_visibility_alter_task_visibility_and_more.py",
      "backend/tasks/migrations/0019_add_search_vector_task.py",
      "backend/tasks/migrations/0020_task_idx_task_search.py",
      "backend/tasks/migrations/0021_enable_rls.py",
      "backend/tasks/migrations/0022_add_task_decisions.py",
      "backend/tasks/migrations/0023_convert_task_decision_to_task_type.py",
      "backend/tasks/migrations/0024_enforce_single_pending_task_decision.py",
      "backend/tasks/migrations/0025_alter_task_status_alter_task_type.py",
      "backend/tasks/migrations/__init__.py",
      "backend/templates",
      "backend/test-results",
      "backend/tests",
      "backend/tests/admin/test_admin_notifications_policy_rbac.py",
      "backend/tests/admin/test_admin_permission_audit.py",
      "backend/tests/admin/test_admin_permission_audit_correlation.py",
      "backend/tests/admin/test_admin_roles_rbac_edit_allow.py",
      "backend/tests/admin/test_admin_roles_rbac_edit_deny.py",
      "backend/tests/admin/test_admin_users_rbac_allow.py",
      "backend/tests/audit/test_audit_events_rbac_deny_audit.py",
      "backend/tests/audit/test_audit_export_rbac_deny.py",
      "backend/tests/audit/test_audit_export_rbac_superuser.py",
      "backend/tests/audit/test_audit_rbac.py",
      "backend/tests/audit/test_audit_rbac_endpoints.py",
      "backend/tests/audit/test_audit_registry_billing_vendor_required.py",
      "backend/tests/audit/test_audit_registry_permissions_required.py",
      "backend/tests/critical/test_auth_oidc.py",
      "backend/tests/integration/test_auth0_idp_asset_bootstrap.py",
      "backend/tests/integration/test_collaboration_authorization.py",
      "backend/tests/integration/test_db_viewer_rbac_allow.py",
      "backend/tests/integration/test_schema_permissions.py",
      "backend/tests/integration/test_schema_ui_permissions.py",
      "backend/tests/integration/test_secret_hashing.py",
      "backend/tests/integration/test_teams_rbac.py",
      "backend/tests/integration/test_teams_rbac_allow.py",
      "backend/tests/integration/test_thirdparty_authorization.py",
      "backend/tests/integration/test_thirdparty_relationship_authorization.py",
      "backend/tests/security/test_auth_flows_comprehensive.py",
      "backend/tests/security/test_auth_login_ratelimit.py",
      "backend/tests/security/test_auth_logout_csrf.py",
      "backend/tests/security/test_auth_session.py",
      "backend/tests/security/test_impersonation_rbac.py",
      "backend/tests/security/test_rbac_admin_api.py",
      "backend/tests/security/test_rbac_casl_mapping.py",
      "backend/tests/security/test_rbac_forbidden_json.py",
      "backend/tests/security/test_rbac_forbidden_json_shape.py",
      "backend/tests/security/test_rbac_risk_matrix.py",
      "backend/tests/security/test_rbac_risk_recalc_command.py",
      "backend/tests/security/test_rbac_settings_guard.py",
      "backend/tests/security/test_thirdparty_unauth_endpoints.py",
      "backend/tests/suppliers/test_suppliers_reports_rbac.py",
      "backend/thirdparties",
      "backend/thirdparties/migrations/0001_initial.py",
      "backend/thirdparties/migrations/0002_enable_rls_policies.py",
      "backend/thirdparties/migrations/0003_bitemporal_constraints.py",
      "backend/thirdparties/migrations/0004_rename_tables_suppliers_to_thirdparties.py",
      "backend/thirdparties/migrations/0005_rename_thirdparties_a_tenant__c63003_idx_thirdpartie_tenant__0c77fd_idx_and_more.py",
      "backend/thirdparties/migrations/0006_rename_thirdparties_a_tenant__c63003_idx_thirdpartie_tenant__0c77fd_idx_and_more.py",
      "backend/thirdparties/migrations/0007_add_asset_service_offering.py",
      "backend/thirdparties/migrations/0008_rename_thirdparties_a_tenant__c63003_idx_thirdpartie_tenant__0c77fd_idx_and_more.py",
      "backend/thirdparties/migrations/0009_supplier_graph_view.py",
      "backend/thirdparties/migrations/0010_add_missing_fields.py",
      "backend/thirdparties/migrations/0011_enable_rls.py",
      "backend/thirdparties/migrations/0012_remove_thirdpartyrelationship_thirdparty_rel_unique_and_more.py",
      "backend/thirdparties/migrations/0012_suppliers_saved_view.py",
      "backend/thirdparties/migrations/0013_asset_service_offering_constraints.py",
      "backend/thirdparties/migrations/0014_check_constraints.py",
      "backend/thirdparties/migrations/0015_performance_indexes.py",
      "backend/thirdparties/migrations/0016_partial_indexes.py",
      "backend/thirdparties/migrations/0017_add_frontend_aligned_fields.py",
      "backend/thirdparties/migrations/0018_add_gdpr_data_privacy_models.py",
      "backend/thirdparties/migrations/0019_remove_dataprivacycontact_created_by_and_more.py",
      "backend/thirdparties/migrations/0020_add_privacy_enhancements.py",
      "backend/thirdparties/migrations/0021_remove_dataprivacycontact_created_by_and_more.py",
      "backend/thirdparties/migrations/0022_repair_privacy_columns.py",
      "backend/thirdparties/migrations/0023_remove_asset_asset_tenant_type_idx_and_more.py",
      "backend/thirdparties/migrations/0024_remove_asset_asset_tenant_type_idx_and_more.py",
      "backend/thirdparties/migrations/0025_alter_document_content_type_and_more.py",
      "backend/thirdparties/migrations/0026_supplierassessment_supplierchangerequest_and_more.py",
      "backend/thirdparties/migrations/0027_dataprivacycontact_dataprivacyprofile_and_more.py",
      "backend/thirdparties/migrations/0028_merge_20251023_1923.py",
      "backend/thirdparties/migrations/0029_tprm_policy_owner_team_doc_source.py",
      "backend/thirdparties/migrations/0030_rename_tp_tenant_owner_user_idx_thirdpartie_tenant__4ef8e4_idx_and_more.py",
      "backend/thirdparties/migrations/0031_add_business_security_ownership.py",
      "backend/thirdparties/migrations/0032_dataprivacycontact_dataprivacyprofile_and_more.py",
      "backend/thirdparties/migrations/0033_alter_thirdparty_visibility.py",
      "backend/thirdparties/migrations/0034_alter_thirdparty_relationship_types.py",
      "backend/thirdparties/migrations/0035_thirdparty_frameworks_alter_thirdparty_tags.py",
      "backend/thirdparties/migrations/0036_alter_thirdparty_tags.py",
      "backend/thirdparties/migrations/0037_add_composite_indexes.py",
      "backend/thirdparties/migrations/0037_add_performance_indexes.py",
      "backend/thirdparties/migrations/0038_merge_20251104_0914.py",
      "backend/thirdparties/migrations/0039_remove_directorylinkconfig_tp_link_sync_idx_and_more.py",
      "backend/thirdparties/migrations/0041_search_extensions_and_indexes.py",
      "backend/thirdparties/migrations/0042_add_search_vector_thirdparty.py",
      "backend/thirdparties/migrations/0043_thirdparty_idx_thirdparty_search.py",
      "backend/thirdparties/migrations/0044_thirdparty_entity_versioning.py",
      "backend/thirdparties/migrations/0045_dataprivacyprofile_third_party_entity.py",
      "backend/thirdparties/migrations/0046_rename_thirdparty_tenant_entity_idx_thirdpartie_tenant__205199_idx_and_more.py",
      "backend/thirdparties/migrations/0047_standardize_rls_gucs.py",
      "backend/thirdparties/migrations/0048_alter_directorylinkconfig_linked_legal_entity_and_more.py",
      "backend/thirdparties/migrations/0049_fix_thirdparty_no_overlap_valid_to_infinity.py",
      "backend/thirdparties/migrations/0050_bitemporal_table_maintenance_tuning.py",
      "backend/thirdparties/migrations/0051_standardize_risk_fields.py",
      "backend/thirdparties/migrations/0052_alter_thirdparty_risk_factors_and_more.py",
      "backend/thirdparties/migrations/0053_directorylinkconfig_linked_local_provider.py",
      "backend/thirdparties/migrations/0054_alter_thirdparty_lifecycle_status.py",
      "backend/thirdparties/migrations/0055_thirdparty_adn_parity_fields.py",
      "backend/thirdparties/migrations/0056_functionalrole_industrycodecrosswalk_and_more.py",
      "backend/thirdparties/migrations/0057_seed_functional_roles.py",
      "backend/thirdparties/migrations/0058_seed_industry_crosswalk.py",
      "backend/thirdparties/migrations/0059_thirdparty_adn_parity_fields.py",
      "backend/thirdparties/migrations/0060_supplier_directory_category.py",
      "backend/thirdparties/migrations/0061_thirdparty_control_frameworks.py",
      "backend/thirdparties/migrations/0062_migrate_frameworks_m2m.py",
      "backend/thirdparties/migrations/0063_alter_thirdparty_frameworks.py",
      "backend/thirdparties/migrations/FOLDER.migrations.md",
      "backend/thirdparties/migrations/__init__.py",
      "backend/webhooks",
      "backend/webhooks/migrations/0001_initial.py",
      "backend/webhooks/migrations/0002_rename_webhooks_de_subscri_f5d8c1_idx_webhooks_de_subscri_f97236_idx_and_more.py",
      "backend/webhooks/migrations/0003_unique_constraints.py",
      "backend/webhooks/migrations/0004_add_owned_resource_mixin.py",
      "backend/webhooks/migrations/0005_add_business_security_ownership.py",
      "backend/webhooks/migrations/0006_alter_webhookdelivery_visibility_and_more.py",
      "backend/webhooks/migrations/0007_hash_webhook_secrets.py",
      "backend/webhooks/migrations/0007_hash_webhook_secrets.py",
      "backend/webhooks/migrations/0008_alter_webhooksubscription_secret.py",
      "backend/webhooks/migrations/0008_alter_webhooksubscription_secret.py",
      "backend/webhooks/migrations/__init__.py",
      "catalog",
      "config",
      "config/bundle",
      "config/lighthouse",
      "config/observability",
      "config/quality",
      "contracts",
      "contracts/config",
      "contracts/migration-to-schemathesis.md",
      "devops",
      "devops/grafana/dashboards/rbac-dashboard.json",
      "devops/prometheus/rules/rbac-alerts.yml",
      "docker",
      "docs",
      "docs/access-auth.md",
      "docs/adr",
      "docs/agents",
      "docs/agents/context/admin.billing.md",
      "docs/api",
      "docs/api/rbac-api-reference.md",
      "docs/api/rbac-api.md",
      "docs/api/rbac-openapi.json",
      "docs/api/rbac-quick-reference.md",
      "docs/architecture",
      "docs/architecture/architecture-deployment.md",
      "docs/architecture/rbac-architecture.md",
      "docs/architecture/security-audit-rbac.md",
      "docs/badges",
      "docs/collaboration",
      "docs/contracts",
      "docs/db",
      "docs/design-system",
      "docs/design-system/automated-deployment-setup.md",
      "docs/design-system/design-tokens-tier-guide.md",
      "docs/development",
      "docs/docker",
      "docs/engineering",
      "docs/feature-flags/catalog-key-migration.md",
      "docs/feature-specs",
      "docs/feature-specs/admin/page-actions/09-deployment-guide.md",
      "docs/feature-specs/controls/deployment-checklist.md",
      "docs/feature-specs/information/my-environment-rbac-integration.md",
      "docs/feature-specs/rbac/admin-roles-review-and-cleanup.md",
      "docs/feature-specs/rbac/rbac-spec.md",
      "docs/feature-specs/search-deployment-guide.md",
      "docs/guides",
      "docs/guides/authentication-setup.md",
      "docs/guides/cost-optimized-deployment.md",
      "docs/guides/deployment-guide-permissions.md",
      "docs/guides/deployment-guide-permissions.md",
      "docs/guides/multi-tenant-deployment-critical.md",
      "docs/guides/post-deployment-setup.md",
      "docs/guides/post-deployment-verification.md",
      "docs/guides/rbac-admin-guide.md",
      "docs/observability",
      "docs/onboarding",
      "docs/openapi",
      "docs/otlp",
      "docs/performance",
      "docs/permissions.md",
      "docs/planning",
      "docs/plans",
      "docs/plans/infrastructure-options-comparison.md",
      "docs/prd",
      "docs/prd/rbac-simplified-design.md",
      "docs/rbac-cache-implementation.md",
      "docs/reference",
      "docs/reference/rbac.yaml",
      "docs/reference/reference-rbac-permission-sync.md",
      "docs/reports",
      "docs/runbooks",
      "docs/runbooks/deploy-admin.md",
      "docs/runbooks/deployment-checklist.md",
      "docs/runbooks/rbac-operations-runbook.md",
      "docs/runbooks/rbac-risk-policy.md",
      "docs/runbooks/runbook-deployment-best-practices.md",
      "docs/runbooks/runbook-production-deployment.md",
      "docs/secret-management-plan.md",
      "docs/security",
      "docs/security/rbac-risk-policy.md",
      "docs/testing",
      "e2e",
      "e2e/fixtures",
      "e2e/fixtures/auth.fixture.ts",
      "e2e/page-objects",
      "e2e/page-objects/auth/login.page.ts",
      "e2e/tests/auth/authentication.spec.ts",
      "functions",
      "gcp-run-proxy",
      "gcp-run-proxy/src",
      "gcp-run-proxy/test",
      "grafana-provisioning",
      "load_tests",
      "logs",
      "manual-deployment-steps.md",
      "migration-complete.md",
      "migration-status.md",
      "migrations-applied-success.md",
      "output",
      "packages",
      "packages/app-shared",
      "packages/app-shared/src/app/AuthenticatedApp.tsx",
      "packages/app-shared/src/auth/AbilityContext.shared.ts",
      "packages/app-shared/src/auth/AbilityProvider.ts",
      "packages/app-shared/src/auth/AbilityProviderRoot.tsx",
      "packages/app-shared/src/auth/AuthError.tsx",
      "packages/app-shared/src/auth/FOLDER.auth.md",
      "packages/app-shared/src/auth/NoTenantAccess.tsx",
      "packages/app-shared/src/auth/SessionExpiryWarningProvider.tsx",
      "packages/app-shared/src/auth/SessionGate.tsx",
      "packages/app-shared/src/auth/UseEnvironmentPermissions.tsx",
      "packages/app-shared/src/auth/UseEnvironmentPermissions.tsx",
      "packages/app-shared/src/auth/__tests__/FOLDER.__tests__.md",
      "packages/app-shared/src/auth/__tests__/permissionGrouping.test.ts",
      "packages/app-shared/src/auth/__tests__/permissionGrouping.test.ts",
      "packages/app-shared/src/auth/ability.ts",
      "packages/app-shared/src/auth/can.ts",
      "packages/app-shared/src/auth/logoutBroadcast.ts",
      "packages/app-shared/src/auth/logoutClient.ts",
      "packages/app-shared/src/auth/permissionGrouping.ts",
      "packages/app-shared/src/auth/permissionGrouping.ts",
      "packages/app-shared/src/auth/rbac-canonical.ts",
      "packages/app-shared/src/auth/rbac-canonical.ts",
      "packages/app-shared/src/auth/session.ts",
      "packages/app-shared/src/auth/sessionExpiryWarningContext.ts",
      "packages/app-shared/src/auth/useSessionHeartbeat.ts",
      "packages/app-shared/src/components/admin/AdminBillingView.tsx",
      "packages/app-shared/src/components/admin/OrgBillingOverviewView.tsx",
      "packages/app-shared/src/components/admin/TenantBillingTab.tsx",
      "packages/app-shared/src/components/admin/roles/BatchPermissionUpdates.tsx",
      "packages/app-shared/src/components/admin/roles/PermissionConflictDetector.tsx",
      "packages/app-shared/src/components/admin/roles/PermissionMatrix.tsx",
      "packages/app-shared/src/components/admin/roles/PermissionMatrixSkeleton.tsx",
      "packages/app-shared/src/components/admin/roles/PermissionsList.tsx",
      "packages/app-shared/src/components/admin/roles/__tests__/BatchPermissionUpdates.test.tsx",
      "packages/app-shared/src/components/auth/PermissionDenied.tsx",
      "packages/app-shared/src/components/auth/PermissionDenied.tsx",
      "packages/app-shared/src/constants/rbac-module-settings.md",
      "packages/app-shared/src/constants/rbac.ts",
      "packages/app-shared/src/features/admin/components/AdminBillingView.tsx",
      "packages/app-shared/src/features/admin/components/roles/PermissionConflictDetector.tsx",
      "packages/app-shared/src/features/admin/components/roles/PermissionMatrix.tsx",
      "packages/app-shared/src/features/admin/components/roles/PermissionMatrixSkeleton.tsx",
      "packages/app-shared/src/features/admin/components/roles/permissionConflictRules.ts",
      "packages/app-shared/src/features/admin/components/roles/permissionMatrix.shared.ts",
      "packages/app-shared/src/features/admin/hooks/useUsersAndPermissions.ts",
      "packages/app-shared/src/features/admin/pages/AdminBillingPage.tsx",
      "packages/app-shared/src/features/admin/pages/AdminMigrationDashboardPage.tsx",
      "packages/app-shared/src/features/admin/pages/AdminMigrationDashboardPageView.tsx",
      "packages/app-shared/src/features/auth/components/PermissionDenied.tsx",
      "packages/app-shared/src/features/auth/components/PermissionDenied.tsx",
      "packages/app-shared/src/features/auth/components/index.ts",
      "packages/app-shared/src/features/auth/index.ts",
      "packages/app-shared/src/features/auth/utils/ability.ts",
      "packages/app-shared/src/features/auth/utils/can.ts",
      "packages/app-shared/src/features/auth/utils/index.ts",
      "packages/app-shared/src/features/auth/utils/permissionGrouping.ts",
      "packages/app-shared/src/features/auth/utils/permissionGrouping.ts",
      "packages/app-shared/src/features/auth/utils/session.ts",
      "packages/app-shared/src/features/information/hooks/useMigration.ts",
      "packages/app-shared/src/features/information/pages/MigrationConflictsPage.tsx",
      "packages/app-shared/src/features/information/pages/MigrationDashboardPage.tsx",
      "packages/app-shared/src/features/information/pages/MigrationImportPage.tsx",
      "packages/app-shared/src/features/org/pages/OrgBillingPage.tsx",
      "packages/app-shared/src/hooks/admin/usePermissionsCatalog.ts",
      "packages/app-shared/src/hooks/admin/useUsersAndPermissions.ts",
      "packages/app-shared/src/hooks/information/useMigration.ts",
      "packages/app-shared/src/hooks/lib/parsePermissions.ts",
      "packages/app-shared/src/hooks/permissions/__tests__/useCanAccess.test.tsx",
      "packages/app-shared/src/hooks/permissions/__tests__/usePermission.test.tsx",
      "packages/app-shared/src/hooks/permissions/index.ts",
      "packages/app-shared/src/hooks/permissions/testUtils.tsx",
      "packages/app-shared/src/hooks/permissions/useAbility.ts",
      "packages/app-shared/src/hooks/permissions/useCanAccess.ts",
      "packages/app-shared/src/hooks/permissions/usePermission.ts",
      "packages/app-shared/src/hooks/useOrgBillingApi.ts",
      "packages/app-shared/src/lib/__tests__/permissions.test.ts",
      "packages/app-shared/src/lib/permissions.ts",
      "packages/app-shared/src/lib/personalTokensApi.ts",
      "packages/app-shared/src/pages/AuthLogoutPage.tsx",
      "packages/app-shared/src/pages/platform/AuthAnalyticsPage.impl.tsx",
      "packages/app-shared/src/pages/platform/AuthAnalyticsPage.tsx",
      "packages/app-shared/src/preauth/debug.ts",
      "packages/app-shared/src/preauth/index.ts",
      "packages/app-shared/src/preauth/network.ts",
      "packages/app-shared/src/preauth/session.ts",
      "packages/app-shared/src/preauth/telemetry.ts",
      "packages/app-shared/src/preauth/theme.ts",
      "packages/app-shared/src/preauth/types.ts",
      "packages/app-shared/src/preauth/ui.ts",
      "packages/app-shared/src/preauth/utils.test.ts",
      "packages/app-shared/src/preauth/utils.ts",
      "packages/app-shared/src/router/Unauthorized.tsx",
      "packages/app-shared/src/tests/admin.billing.a11y.test.tsx",
      "packages/app-shared/src/tests/admin.billing.exportmenu.test.tsx",
      "packages/app-shared/src/tests/admin.billing.mobile.test.tsx",
      "packages/app-shared/src/tests/admin.billing.toolbar.smoke.test.tsx",
      "packages/app-shared/src/tests/admin.users.rbac.banner.test.tsx",
      "packages/app-shared/src/tests/api.credentials.test.ts",
      "packages/app-shared/src/tests/auth.can.test.ts",
      "packages/app-shared/src/tests/permission.gate.test.tsx",
      "packages/app-shared/src/tests/router.unauthorized.ui.test.tsx",
      "packages/app-shared/src/tests/suppliers.directory.views.rbac.test.tsx",
      "packages/app-shared/src/types/rbac.ts",
      "packages/auth",
      "packages/auth/package.json",
      "packages/auth/src/__tests__/permissionGrouping.test.ts",
      "packages/auth/src/__tests__/permissionGrouping.test.ts",
      "packages/auth/src/__tests__/rbac-canonical.test.ts",
      "packages/auth/src/__tests__/rbac-canonical.test.ts",
      "packages/auth/src/ability.ts",
      "packages/auth/src/can.ts",
      "packages/auth/src/index.ts",
      "packages/auth/src/logout/broadcast.ts",
      "packages/auth/src/logout/client.ts",
      "packages/auth/src/logout/index.ts",
      "packages/auth/src/permissionGrouping.ts",
      "packages/auth/src/permissionGrouping.ts",
      "packages/auth/src/rbac-canonical.ts",
      "packages/auth/src/rbac-canonical.ts",
      "packages/auth/src/session.ts",
      "packages/auth/test-results/junit.xml",
      "packages/auth/tsconfig.json",
      "packages/auth/tsconfig.tsbuildinfo",
      "packages/config",
      "packages/documentation/migration/page-checklist.json",
      "packages/eslint-plugin-aeptus",
      "packages/types",
      "packages/types/src/rbac.ts",
      "packages/ui",
      "packages/ui/.ai/design-tokens.json",
      "packages/ui/.ai/migration-rules.json",
      "packages/ui/src/components/molecules/TokenPicker/TokenPicker.tsx",
      "packages/ui/src/components/molecules/TokenPicker/index.ts",
      "packages/ui/src/tokens/components.css",
      "packages/ui/src/tokens/index.css",
      "packages/ui/src/tokens/index.ts",
      "packages/ui/src/tokens/primitives.css",
      "packages/ui/src/tokens/semantic.css",
      "packages/ui/src/tokens/themes/dark.css",
      "patches",
      "playwright-report",
      "postgres-18-migration-guide.md",
      "project",
      "public",
      "rbac-cache-delivery.md",
      "rbac-cache-quickstart.md",
      "scripts",
      "scripts/a11y",
      "scripts/adr",
      "scripts/ai",
      "scripts/archive",
      "scripts/assets",
      "scripts/catalog",
      "scripts/checks",
      "scripts/checks/check-customer-preauth-no-design-system.mjs",
      "scripts/ci",
      "scripts/ci/check-endpoint-permissions.mjs",
      "scripts/ci/check-permission-metadata.mjs",
      "scripts/ci/check-route-permissions.sh",
      "scripts/ci/check_migrations.sh",
      "scripts/ci/validate-rbac-sync.mjs",
      "scripts/contracts",
      "scripts/deploy-types.cjs",
      "scripts/deployment/build-production.sh",
      "scripts/design-system",
      "scripts/design-system/generate-token-json.mjs",
      "scripts/dev",
      "scripts/docs",
      "scripts/generate",
      "scripts/help",
      "scripts/i18n",
      "scripts/k6",
      "scripts/maintenance",
      "scripts/migration/audit-page-components.mjs",
      "scripts/naming",
      "scripts/observability",
      "scripts/openapi",
      "scripts/perf",
      "scripts/security",
      "scripts/tools",
      "scripts/trace",
      "scripts/validate-deployment.sh",
      "scripts/validation",
      "scripts/validation/validate_permissions.py",
      "scripts/verify-phase0-deployment.sh",
      "scripts/verify_migration.sh",
      "scripts/ws",
      "shared",
      "src",
      "src/i18n",
      "stories",
      "test-results",
      "tests",
      "tests/contract",
      "tests/contract/consumers/auth.contract.test.ts",
      "tools",
      "tools/mcp-mordor",
      "tools/mcp-mordor/src/tools/rbac.ts"
    ],
    "risks": [
      ".gcloud_access_token",
      ".github/workflows/migrations-guard.yml",
      ".pnpm-store/v10/index/17/3659f9b86de57d0529eeccc33dc3015026947d415796e549a93f9473012b3d-oauth4webapi@3.8.2.json",
      ".pnpm-store/v10/index/18/7b8344ed764b2a6ed9c57bd1dd5d900d845265c7827b6bcdba6f381f90cbee-comma-separated-tokens@1.0.8.json",
      ".pnpm-store/v10/index/29/afbd4ebbadbfb1bc33a593e927a2456cfbf762b9a84a881841b35ca84013ac-class-variance-authority@0.7.1.json",
      ".pnpm-store/v10/index/45/d2547e5704ddc5332a232a420b02bb4e853eef5474824ed1b7986cf8473789-js-tokens@4.0.0.json",
      ".pnpm-store/v10/index/55/dffd1150e2bba3cf26df72021eaba193fa125d711eb76f2151a3c81b074744-@csstools+css-tokenizer@3.0.4.json",
      ".pnpm-store/v10/index/59/dee61cf43ff33cba423edfe13e3abe0ddaa28afc7ec9099ba8366728f4eb8a-@auth+core@0.41.0.json",
      ".pnpm-store/v10/index/9b/16bd13d21314eb746da9f78fa2f93298f07a01b3ea505098cd4826459e0591-js-tokens@9.0.1.json",
      ".pnpm-store/v10/index/a3/69ee27ce43e04491c9b877cdb0390e5d4e7b5edf4592fefd0d7b6f5a90752f-@auth0+auth0-react@2.5.0.json",
      ".pnpm-store/v10/index/ab/f25255dd4ba6dce17f96e4626e286f88963e3c742a245edec44504dad5a9b2-space-separated-tokens@1.1.5.json",
      ".pnpm-store/v10/index/e1/7bf1d84e0dd808abaf5469f8a39e8dd0dba63e4b9df2ed359fd368e768ed56-@auth0+auth0-spa-js@2.5.0.json",
      ".pnpm-store/v10/index/f9/ce7582ab8cdc5ea73159a802eb1127b448a18d0ae13b3d1c20b0cb2fc14687-next-auth@5.0.0-beta.30.json",
      ".pnpm-store/v10/index/ff/b05db84885788349ee695cf22466aa9d2c0f0d9ada50056a18a0fd11a9a67e-eslint-plugin-no-secrets@2.2.1.json",
      ".secrets.baseline",
      "Agents/skills/auth/SKILL.md",
      "Agents/skills/auth/references/api-endpoints.md",
      "Agents/skills/auth/references/api-keys.md",
      "Agents/skills/auth/references/authentication.md",
      "Agents/skills/auth/references/common-patterns.md",
      "Agents/skills/auth/references/database-tables.md",
      "Agents/skills/auth/references/decisions.md",
      "Agents/skills/auth/references/learn-log.md",
      "Agents/skills/auth/references/rbac.md",
      "Agents/skills/auth/references/rbac.md",
      "Agents/skills/auth/references/security.md",
      "Agents/skills/auth/references/troubleshooting.md",
      "Agents/skills/ci-deploy/SKILL.md",
      "Agents/skills/ci-deploy/references/advanced-pipelines.md",
      "Agents/skills/ci-deploy/references/decisions.md",
      "Agents/skills/ci-deploy/references/docker.md",
      "Agents/skills/ci-deploy/references/gcp.md",
      "Agents/skills/ci-deploy/references/kubernetes.md",
      "Agents/skills/ci-deploy/references/learn-log.md",
      "Agents/skills/ci-deploy/references/pipelines.md",
      "Agents/skills/ci-deploy/references/secrets.md",
      "Agents/skills/ci-deploy/references/secrets.md",
      "Agents/skills/database/references/migrations.md",
      "Agents/skills/integrations/references/oauth-flows.md",
      "Agents/tasks/2025-01-13-integrations-onboarding-oauth.md",
      "Agents/tasks/celery-cloudbuild-deploy.md",
      "Agents/tasks/celery-redis-secret-wiring.md",
      "Agents/tasks/dedicated-repo-migration.md",
      "Agents/tasks/fix-bootstrap-permission-case.md",
      "Agents/tasks/fix-environment-discovery-migration.md",
      "Agents/tasks/fix-mordor-roles-permissions-404.md",
      "Agents/tasks/fix-preauth-error-production.md",
      "Agents/tasks/google-oauth-onboarding.md",
      "Agents/tasks/merge-environment-0036-migrations.md",
      "Agents/tasks/otel-step1-deployment.md",
      "Agents/tasks/rbac-implementation-plan-intake.md",
      "Agents/tasks/rbac-pr5-pr8.md",
      "Agents/tasks/rbac-role-management-cleanup.md",
      "Agents/tasks/rbac-role-management-permissions.md",
      "Agents/tasks/role-management-permissions-check.md",
      "apps/customer/src/entry-authenticated.tsx",
      "apps/mordor/src/entry-authenticated.tsx",
      "apps/organizations/src/entry-authenticated.tsx",
      "backend/MIGRATION_SCRIPT.py",
      "backend/accounts/admin_rbac_api_views.py",
      "backend/accounts/admin_rbac_views.py",
      "backend/accounts/auth0_management.py",
      "backend/accounts/auth_analytics_models.py",
      "backend/accounts/auth_analytics_serializers.py",
      "backend/accounts/auth_analytics_views.py",
      "backend/accounts/management/commands/rbac_dump_casl_catalog.py",
      "backend/accounts/management/commands/rbac_lifecycle_tick.py",
      "backend/accounts/management/commands/rbac_roles_summary.py",
      "backend/accounts/management/commands/rbac_seed_permissions.py",
      "backend/accounts/middleware_auth_enforcement.py",
      "backend/accounts/middleware_rbac_identity.py",
      "backend/accounts/migrations/0001_initial.py",
      "backend/accounts/migrations/0002_organization.py",
      "backend/accounts/migrations/0003_userprofile_org_default.py",
      "backend/accounts/migrations/0004_rls_userprofile.py",
      "backend/accounts/migrations/0005_tenant_membership.py",
      "backend/accounts/migrations/0006_userprofile_tenant_nullable.py",
      "backend/accounts/migrations/0007_seed_default_tenants_assign.py",
      "backend/accounts/migrations/0008_userprofile_tenant_nonnull.py",
      "backend/accounts/migrations/0009_rls_userprofile_tenant_update.py",
      "backend/accounts/migrations/0010_alter_userprofile_organization_and_more.py",
      "backend/accounts/migrations/0011_profile_identity_fields.py",
      "backend/accounts/migrations/0012_profile_phone_split.py",
      "backend/accounts/migrations/0013_team_and_identity_extras.py",
      "backend/accounts/migrations/0014_team_id_default.py",
      "backend/accounts/migrations/0015_userprofile_notification_prefs.py",
      "backend/accounts/migrations/0016_userprofile_tz_locale_notif_state.py",
      "backend/accounts/migrations/0017_tenant_notification_policy.py",
      "backend/accounts/migrations/0018_tenant_lifecycle_and_admin_models.py",
      "backend/accounts/migrations/0019_plan_entitlements.py",
      "backend/accounts/migrations/0020_alter_plandefinition_id_and_more.py",
      "backend/accounts/migrations/0021_internal_scopes_and_profile_flag.py",
      "backend/accounts/migrations/0022_custom_attributes.py",
      "backend/accounts/migrations/0023_team_user_custom.py",
      "backend/accounts/migrations/0024_rbac_registry.py",
      "backend/accounts/migrations/0024_rbac_registry.py",
      "backend/accounts/migrations/0025_role_archive.py",
      "backend/accounts/migrations/0025_search_trgm_indexes.py",
      "backend/accounts/migrations/0026_alter_customattributedefinition_id.py",
      "backend/accounts/migrations/0027_merge_20250922_0837.py",
      "backend/accounts/migrations/0028_permission_meta.py",
      "backend/accounts/migrations/0028_permission_meta.py",
      "backend/accounts/migrations/0028_role_risk_fields.py",
      "backend/accounts/migrations/0029_permission_metadata.py",
      "backend/accounts/migrations/0029_permission_metadata.py",
      "backend/accounts/migrations/0030_userprofile_ui_prefs.py",
      "backend/accounts/migrations/0031_enable_tenant_rls.py",
      "backend/accounts/migrations/0032_organization_hierarchy.py",
      "backend/accounts/migrations/0033_remove_organization_org_parent_idx_and_more.py",
      "backend/accounts/migrations/0034_check_constraints.py",
      "backend/accounts/migrations/0035_organization_profile_fields.py",
      "backend/accounts/migrations/0036_grc_organization_fields.py",
      "backend/accounts/migrations/0037_remove_sso_mfa_fields.py",
      "backend/accounts/migrations/0038_alter_organization_tax_id.py",
      "backend/accounts/migrations/0039_tenant_api_calls_month_tenant_api_calls_today_and_more.py",
      "backend/accounts/migrations/0040_tenant_admin_notification_message_and_more.py",
      "backend/accounts/migrations/0041_rolev2_organization_parent_userprofile_primary_team_and_more.py",
      "backend/accounts/migrations/0042_tenanthealthalertrule_tenanthealthmetric_and_more.py",
      "backend/accounts/migrations/0043_broadcasttemplate_scheduledbroadcast_and_more.py",
      "backend/accounts/migrations/0044_alter_scopedpermission_resource_roletemplate.py",
      "backend/accounts/migrations/0044_alter_scopedpermission_resource_roletemplate.py",
      "backend/accounts/migrations/0045_rolev2_tags.py",
      "backend/accounts/migrations/0046_remove_business_unit_and_update_team_types.py",
      "backend/accounts/migrations/0047_sync_rbac_permissions.py",
      "backend/accounts/migrations/0047_sync_rbac_permissions.py",
      "backend/accounts/migrations/0048_remove_userprofile_role.py",
      "backend/accounts/migrations/0049_alter_scopedpermission_resource.py",
      "backend/accounts/migrations/0049_alter_scopedpermission_resource.py",
      "backend/accounts/migrations/0999_rename_rolev2_to_role.py",
      "backend/accounts/migrations/1000_alter_role_options_alter_role_tenant.py",
      "backend/accounts/migrations/1001_drop_legacy_rbac_tables.py",
      "backend/accounts/migrations/1001_drop_legacy_rbac_tables.py",
      "backend/accounts/migrations/1002_recreate_role_permissions_through_table.py",
      "backend/accounts/migrations/1002_recreate_role_permissions_through_table.py",
      "backend/accounts/migrations/1003_alter_scopedpermission_resource.py",
      "backend/accounts/migrations/1003_alter_scopedpermission_resource.py",
      "backend/accounts/migrations/1004_alter_scopedpermission_action.py",
      "backend/accounts/migrations/1004_alter_scopedpermission_action.py",
      "backend/accounts/migrations/1005_add_device_and_session_models.py",
      "backend/accounts/migrations/1006_alter_scopedpermission_resource.py",
      "backend/accounts/migrations/1006_alter_scopedpermission_resource.py",
      "backend/accounts/migrations/1007_add_dashboard_resource.py",
      "backend/accounts/migrations/1008_entitlements_catalog.py",
      "backend/accounts/migrations/1009_seed_owner_internal.py",
      "backend/accounts/migrations/1010_subscription_split.py",
      "backend/accounts/migrations/1011_alter_catalogsubscription_id_alter_creditgrant_id_and_more.py",
      "backend/accounts/migrations/1012_merge_20251105_2056.py",
      "backend/accounts/migrations/1013_delete_rolev2_remove_role_archived_and_more.py",
      "backend/accounts/migrations/1014_notification_columns_and_locale_fields.py",
      "backend/accounts/migrations/1015_merge_20251122_2008.py",
      "backend/accounts/migrations/1016_add_account_models.py",
      "backend/accounts/migrations/1017_assign_demo_admin.py",
      "backend/accounts/migrations/1018_remove_demo_fullaccess_prod.py",
      "backend/accounts/migrations/1019_add_integrations_permissions.py",
      "backend/accounts/migrations/1019_add_integrations_permissions.py",
      "backend/accounts/migrations/1020_add_user_search_trgm_indexes.py",
      "backend/accounts/migrations/1021_role_risk_level_role_risk_meta_and_more.py",
      "backend/accounts/migrations/1022_userprofile_rls_by_user_id.py",
      "backend/accounts/migrations/1023_standardize_rls_gucs.py",
      "backend/accounts/migrations/1024_account_assetentity_fk.py",
      "backend/accounts/migrations/1025_account_roles_permissions_auth.py",
      "backend/accounts/migrations/1025_account_roles_permissions_auth.py",
      "backend/accounts/migrations/1025_account_roles_permissions_auth.py",
      "backend/accounts/migrations/1026_remove_account_accounts_ac_tenant__auth_idx_and_more.py",
      "backend/accounts/migrations/1026_remove_account_accounts_ac_tenant__auth_idx_and_more.py",
      "backend/accounts/migrations/1027_add_risk_rules_permissions.py",
      "backend/accounts/migrations/1027_add_risk_rules_permissions.py",
      "backend/accounts/migrations/1028_add_account_risk_fields.py",
      "backend/accounts/migrations/1029_add_finding_template_model.py",
      "backend/accounts/migrations/1030_role_is_template_role_source_template_and_more.py",
      "backend/accounts/migrations/1031_role_templates_global.py",
      "backend/accounts/migrations/1032_remove_role_accounts_role_template_requires_null_tenant_and_more.py",
      "backend/accounts/migrations/1033_drop_scopedpermission_and_legacy_role_fields.py",
      "backend/accounts/migrations/1033_drop_scopedpermission_and_legacy_role_fields.py",
      "backend/accounts/migrations/1034_alter_userroleassignment_scope_type_and_more.py",
      "backend/accounts/migrations/1035_roleriskpolicy.py",
      "backend/accounts/migrations/1036_account_oauth_scopes.py",
      "backend/accounts/migrations/1036_account_oauth_scopes.py",
      "backend/accounts/migrations/1037_add_external_avatar_url.py",
      "backend/accounts/migrations/1038_grant_demo_admin_v3.py",
      "backend/accounts/migrations/1039_rbac_homogenization.py",
      "backend/accounts/migrations/1039_rbac_homogenization.py",
      "backend/accounts/migrations/1040_remove_permissionauditlog_actor_and_more.py",
      "backend/accounts/migrations/1040_remove_permissionauditlog_actor_and_more.py",
      "backend/accounts/migrations/1041_alter_role_permissions_v3.py",
      "backend/accounts/migrations/1041_alter_role_permissions_v3.py",
      "backend/accounts/migrations/1042_migrate_permissions_to_canonical.py",
      "backend/accounts/migrations/1042_migrate_permissions_to_canonical.py",
      "backend/accounts/migrations/1043_access_grants_and_scope_types.py",
      "backend/accounts/migrations/1044_tenant_slug_global_unique.py",
      "backend/accounts/migrations/1045_rename_accounts_acc_grantor_status_idx_accounts_ac_grantor_970445_idx_and_more.py",
      "backend/accounts/migrations/1046_tenant_onboarding_apps_score_and_more.py",
      "backend/accounts/migrations/1047_tenant_dns_discovery_seed_fields.py",
      "backend/accounts/migrations/1048_authanalyticssummary_authevent.py",
      "backend/accounts/migrations/1048_authanalyticssummary_authevent.py",
      "backend/accounts/migrations/1048_seed_free_plan.py",
      "backend/accounts/migrations/1049_add_domain_role_exposure.py",
      "backend/accounts/migrations/1050_change_domain_role_to_roles_array.py",
      "backend/accounts/migrations/1051_tenant_profiles.py",
      "backend/accounts/migrations/1052_seed_tenant_profiles.py",
      "backend/accounts/migrations/1053_tenant_profile_templates.py",
      "backend/accounts/migrations/1054_seed_tenant_profile_templates.py",
      "backend/accounts/migrations/1055_role_templates_scope_and_profiles.py",
      "backend/accounts/migrations/1056_alter_role_organization_and_more.py",
      "backend/accounts/migrations/1057_tenantdomain_asset_entity.py",
      "backend/accounts/migrations/1058_role_template_visibility_and_auto_create.py",
      "backend/accounts/migrations/1059_fix_account_asset_fk_constraint.py",
      "backend/accounts/migrations/1060_enforce_userprofile_rls.py",
      "backend/accounts/migrations/1061_external_groups.py",
      "backend/accounts/migrations/1062_rename_accounts_ex_tenant__3a632a_idx_accounts_ex_tenant__0c1f4d_idx_and_more.py",
      "backend/accounts/migrations/1063_role_is_platform_staff.py",
      "backend/accounts/migrations/1064_platform_roles.py",
      "backend/accounts/migrations/1065_usersession_realm_enforcement.py",
      "backend/accounts/migrations/1066_remove_platformroleassignment_platform_role_assignment_user_role_uniq_and_more.py",
      "backend/accounts/migrations/1067_consolidate_data_models.py",
      "backend/accounts/migrations/1068_alter_organization_options_alter_team_options_and_more.py",
      "backend/accounts/migrations/1069_documentslot_and_status.py",
      "backend/accounts/migrations/1070_platform_role_assignment_starts_at.py",
      "backend/accounts/migrations/1071_merge_20260202_1350.py",
      "backend/accounts/migrations/1072_seed_default_platform_roles.py",
      "backend/accounts/migrations/1073_feature_key_allow_dots.py",
      "backend/accounts/migrations/1074_aeptus_support_access.py",
      "backend/accounts/migrations/1075_alter_usertenantmembership_role.py",
      "backend/accounts/migrations/1076_userprofile_rls_insert_policy.py",
      "backend/accounts/migrations/1077_migrate_platform_roles_to_canonical_rbac.py",
      "backend/accounts/migrations/1077_migrate_platform_roles_to_canonical_rbac.py",
      "backend/accounts/migrations/1078_userprofile_rls_include_memberships.py",
      "backend/accounts/migrations/1079_userprofile_archived_at.py",
      "backend/accounts/migrations/1080_profile_integrity_jobs.py",
      "backend/accounts/migrations/1081_impersonation_ticket_and_request_id.py",
      "backend/accounts/migrations/1082_alter_tenant_options.py",
      "backend/accounts/migrations/1083_alter_scheduledbroadcast_status.py",
      "backend/accounts/migrations/1084_tenant_profile_fk_and_framework_template.py",
      "backend/accounts/migrations/1085_seed_baseline_framework_templates.py",
      "backend/accounts/migrations/1086_merge_20260305_1932.py",
      "backend/accounts/migrations/FOLDER.migrations.md",
      "backend/accounts/migrations/__init__.py",
      "backend/accounts/permissions_base.py",
      "backend/accounts/rbac.py",
      "backend/accounts/rbac_audit_models.py",
      "backend/accounts/rbac_canonical.py",
      "backend/accounts/rbac_helpers.py",
      "backend/accounts/rbac_models.py",
      "backend/accounts/rbac_permissions.py",
      "backend/accounts/rbac_scope.py",
      "backend/accounts/rbac_signals.py",
      "backend/accounts/tests/test_rbac_access_engine.py",
      "backend/accounts/tests/test_rbac_lifecycle_tick.py",
      "backend/accounts/tests/test_rbac_on_behalf_audit.py",
      "backend/accounts/tests/test_rbac_team_auto_assign.py",
      "backend/adn/migrations/0001_initial.py",
      "backend/adn/migrations/0002_enable_rls.py",
      "backend/adn/migrations/0003_fix_category_slug_uniqueness.py",
      "backend/adn/migrations/0004_pipelinerun_enrichmentqueue_directorysignal_and_more.py",
      "backend/adn/migrations/0005_localproviderentry_localserviceentry_and_more.py",
      "backend/adn/migrations/0006_remove_localproviderentry_unique_local_provider_domain_per_tenant_and_more.py",
      "backend/adn/migrations/0007_add_schema_version.py",
      "backend/adn/migrations/0008_directorycategory_expected_at_onboarding.py",
      "backend/adn/migrations/0009_add_app_metadata_facts.py",
      "backend/adn/migrations/0010_expand_fact_types.py",
      "backend/adn/migrations/0011_add_category_owner_fields.py",
      "backend/adn/migrations/0012_pipelinerun_add_adn_onboarding_enrich_stage.py",
      "backend/adn/migrations/0013_category_owner_delegation.py",
      "backend/adn/migrations/0014_pipelinestageconfig.py",
      "backend/adn/migrations/0015_remove_directoryfact_fact_single_target_entity_and_more.py",
      "backend/adn/migrations/0016_sitemap_supply_chain_choice_expansions.py",
      "backend/adn/migrations/0017_rename_enrichmentqueue_pipelinequeue.py",
      "backend/adn/migrations/0018_rename_adn_pipelin_target__70e8a1_idx_adn_pipelin_target__d13f85_idx_and_more.py",
      "backend/adn/migrations/0019_pipelinebatch.py",
      "backend/adn/migrations/0020_rename_adn_pipelin_status_batch_idx_adn_pipelin_status_90c11e_idx_and_more.py",
      "backend/adn/migrations/__init__.py",
      "backend/adn/permissions.py",
      "backend/adn/tests/test_permissions.py",
      "backend/ai_providers/migrations/0001_initial.py",
      "backend/ai_providers/migrations/0002_seed_providers.py",
      "backend/ai_providers/migrations/__init__.py",
      "backend/analytics/migrations/0001_initial.py",
      "backend/analytics/migrations/__init__.py",
      "backend/api_keys/migrations/0001_initial.py",
      "backend/api_keys/migrations/0002_rename_api_keys_tenant__a3f8b1_idx_api_keys_tenant__aa40c3_idx_and_more.py",
      "backend/api_keys/migrations/0003_unique_constraints.py",
      "backend/api_keys/migrations/0004_apikey_user.py",
      "backend/api_keys/migrations/__init__.py",
      "backend/api_usage/migrations/0001_initial.py",
      "backend/api_usage/migrations/0002_enable_rls.py",
      "backend/api_usage/migrations/0003_rename_api_deprec_tenant_status_idx_api_depreca_tenant__60e9d0_idx_and_more.py",
      "backend/api_usage/migrations/0004_brin_indexes.py",
      "backend/api_usage/migrations/0005_merge_20251001_1316.py",
      "backend/api_usage/migrations/0006_standardize_rls_gucs.py",
      "backend/api_usage/migrations/__init__.py",
      "backend/audit/migrations/0001_initial.py",
      "backend/audit/migrations/0002_rls_and_brin.py",
      "backend/audit/migrations/0003_dedup_unique.py",
      "backend/audit/migrations/0003_partition_shadow_table.py",
      "backend/audit/migrations/0004_alter_auditeventv2_options_and_more.py",
      "backend/audit/migrations/0004_audit_export_job.py",
      "backend/audit/migrations/0005_audit_ingest_keys.py",
      "backend/audit/migrations/0005_audit_phase3_enhancements.py",
      "backend/audit/migrations/0006_auditpolicy_legal_hold.py",
      "backend/audit/migrations/0007_auditpolicy_retention_status.py",
      "backend/audit/migrations/0008_auditeventv2_perf_indexes.py",
      "backend/audit/migrations/0009_merge_0004_0008.py",
      "backend/audit/migrations/0010_drop_audit_event_legacy.py",
      "backend/audit/migrations/0011_alter_auditexportjob_id.py",
      "backend/audit/migrations/0012_auditexportjob_expires_at_auditexportjob_format_and_more.py",
      "backend/audit/migrations/0013_standardize_rls_gucs.py",
      "backend/audit/migrations/0014_actor_id_string.py",
      "backend/audit/migrations/FOLDER.migrations.md",
      "backend/audit/migrations/__init__.py",
      "backend/automations/migrations/0001_initial.py",
      "backend/automations/migrations/0002_definition.py",
      "backend/automations/migrations/0003_data_model_rest.py",
      "backend/automations/migrations/0004_run_logs.py",
      "backend/automations/migrations/0005_enable_rls.py",
      "backend/automations/migrations/0006_check_constraints.py",
      "backend/automations/migrations/0007_performance_indexes.py",
      "backend/automations/migrations/0008_brin_indexes.py",
      "backend/automations/migrations/0009_partial_indexes.py",
      "backend/automations/migrations/0010_remove_automationdefinition_auto_def_tenant_status_idx_and_more.py",
      "backend/automations/migrations/0011_merge_20251106_2056.py",
      "backend/automations/migrations/0012_remove_automationdefinition_created_by_and_more.py",
      "backend/automations/migrations/0013_standardize_rls_gucs.py",
      "backend/automations/migrations/0014_eventdeadletter_payload_json.py",
      "backend/automations/migrations/__init__.py",
      "backend/collaboration/migrations/0001_initial.py",
      "backend/collaboration/migrations/0002_review_models.py",
      "backend/collaboration/migrations/0003_rename_collaborati_locatio_idx_collaborati_locatio_8dcb41_idx_and_more.py",
      "backend/collaboration/migrations/__init__.py",
      "backend/community/migrations/0001_initial.py",
      "backend/community/migrations/0002_enable_rls_policies.py",
      "backend/community/migrations/0003_alter_implicitsignal_target_type.py",
      "backend/community/migrations/__init__.py",
      "backend/controls/migrations/0001_initial.py",
      "backend/controls/migrations/0002_performance_indexes.py",
      "backend/controls/migrations/0003_custom_dashboards.py",
      "backend/controls/migrations/0004_remove_controldefinition_ctrl_tenant_status_domain_idx_and_more.py",
      "backend/controls/migrations/0005_add_control_assessment_items.py",
      "backend/controls/migrations/0006_add_scope_dsl_fields.py",
      "backend/controls/migrations/0007_rename_ctrl_item_tenant_occ_idx_controls_co_tenant__9e7728_idx_and_more.py",
      "backend/controls/migrations/0008_access_review_fields.py",
      "backend/controls/migrations/0009_item_validity_fields.py",
      "backend/controls/migrations/0009_rename_controls_co_tenant_c_kind_idx_controls_co_tenant__9e629c_idx_and_more.py",
      "backend/controls/migrations/0010_merge_20251007_1220.py",
      "backend/controls/migrations/0010_occurrence_signoff_fields.py",
      "backend/controls/migrations/0011_merge_20251007_1252.py",
      "backend/controls/migrations/0012_rename_controls_occ_signoff_due_idx_controls_co_signoff_7bb034_idx.py",
      "backend/controls/migrations/0013_controldefinition_business_unit_and_more.py",
      "backend/controls/migrations/0014_add_composite_indexes.py",
      "backend/controls/migrations/0014_add_performance_indexes.py",
      "backend/controls/migrations/0015_merge_20251104_0914.py",
      "backend/controls/migrations/0016_evidence_and_more.py",
      "backend/controls/migrations/0017_add_search_vector_control.py",
      "backend/controls/migrations/0018_controldefinition_idx_control_search.py",
      "backend/controls/migrations/0019_controldefinition_idx_control_search.py",
      "backend/controls/migrations/0020_enable_rls.py",
      "backend/controls/migrations/0021_evidence_artifact_scan_fields.py",
      "backend/controls/migrations/0022_framework_requirement_and_policy_mapping.py",
      "backend/controls/migrations/0023_populate_framework_requirements.py",
      "backend/controls/migrations/0024_rename_controls_fr_framewo_idx_cat_controls_fr_framewo_83042f_idx_and_more.py",
      "backend/controls/migrations/__init__.py",
      "backend/controls/permissions.py",
      "backend/controls/tests/test_permissions.py",
      "backend/controls/tests/test_rbac_boundary.py",
      "backend/core/management/commands/create_search_permissions.py",
      "backend/core/migrations/0001_initial.py",
      "backend/core/migrations/0003_inapp_security_evidence.py",
      "backend/core/migrations/0004_alerts_evidence_meta_url.py",
      "backend/core/migrations/0005_auditevent_healthcheck_and_more.py",
      "backend/core/migrations/0006_outbound_email_job.py",
      "backend/core/migrations/0007_emailjob_partial_idx.py",
      "backend/core/migrations/0008_outbound_email_bodyhash_unique.py",
      "backend/core/migrations/0009_remove_outboundemailjob_core_emailjob_triplet_uniq_and_more.py",
      "backend/core/migrations/0010_pg_stat_statements_extension.py",
      "backend/core/migrations/0011_delete_auditevent.py",
      "backend/core/migrations/0012_drop_core_auditevent.py",
      "backend/core/migrations/0013_rlsauditevent.py",
      "backend/core/migrations/0014_designsystempage_designsystemcomponent_and_more.py",
      "backend/core/migrations/0015_add_planned_components.py",
      "backend/core/migrations/0016_add_resource_permission_models.py",
      "backend/core/migrations/0016_add_resource_permission_models.py",
      "backend/core/migrations/0017_rlsauditevent.py",
      "backend/core/migrations/0018_change_default_visibility_to_tenant.py",
      "backend/core/migrations/0019_tenantattribute_moduleattributeconfig_and_more.py",
      "backend/core/migrations/0020_queryperformancelog_queryperformancestats.py",
      "backend/core/migrations/0021_rename_core_queryp_created_af2bd6_idx_core_queryp_created_ff0917_idx_and_more.py",
      "backend/core/migrations/0022_merge_20251106_2056.py",
      "backend/core/migrations/0023_search_analytics_models.py",
      "backend/core/migrations/0024_alter_searchanalytics_created_by_and_more.py",
      "backend/core/migrations/0025_export_job.py",
      "backend/core/migrations/0026_enable_rls_core_export_job.py",
      "backend/core/migrations/0027_alter_exportjob_format_alter_exportjob_status.py",
      "backend/core/migrations/FOLDER.migrations.md",
      "backend/core/migrations/__init__.py",
      "backend/core/permissions.py",
      "backend/core/permissions/__init__.py",
      "backend/core/permissions/decorators.py",
      "backend/core/permissions/helpers.py",
      "backend/core/permissions/policy.py",
      "backend/core/permissions/rbac.py",
      "backend/core/permissions/rls_queryset_manager.py",
      "backend/core/permissions/test_utils.py",
      "backend/core/permissions/tests/__init__.py",
      "backend/core/permissions/tests/test_decorators.py",
      "backend/core/permissions/tests/test_helpers.py",
      "backend/core/permissions/tests/test_policy.py",
      "backend/core/tests/test_search/test_search_permissions.py",
      "backend/deployment-guide.md",
      "backend/directory/migrations/0001_initial.py",
      "backend/directory/migrations/0002_bitemporal_constraints.py",
      "backend/directory/migrations/0003_add_service_offering_technology.py",
      "backend/directory/migrations/0004_bitemporal_exclusion_constraints.py",
      "backend/directory/migrations/0005_check_constraints.py",
      "backend/directory/migrations/0006_alter_technologycomponent_unique_together_and_more.py",
      "backend/directory/migrations/0007_service_offering_technology_constraints_and_cleanup.py",
      "backend/directory/migrations/0008_legalentity_categories_serviceoffering_categories_and_more.py",
      "backend/directory/migrations/0009_legalentity_serviceoffering_expansion.py",
      "backend/directory/migrations/0010_legalentity_industry_sanctions_jurisdiction.py",
      "backend/directory/migrations/0011_allow_null_legal_entity_on_service_offering.py",
      "backend/directory/migrations/0012_remove_technologycomponent_categories_and_more.py",
      "backend/directory/migrations/0013_fix_techcat_null_distinct.py",
      "backend/directory/migrations/0014_remove_serviceoffering_tags.py",
      "backend/directory/migrations/0015_technologyproduct_categories.py",
      "backend/directory/migrations/0016_technologycategory_is_active.py",
      "backend/directory/migrations/__init__.py",
      "backend/documents/deployment-guide.md",
      "backend/documents/migrations/0001_initial.py",
      "backend/documents/migrations/0002_rename_doc_t_type_deleted_idx_documents_d_tenant__5ef7dd_idx_and_more.py",
      "backend/documents/migrations/0003_enable_rls.py",
      "backend/documents/migrations/0004_expand_doctype_and_relations.py",
      "backend/documents/migrations/0005_documentslot_and_status.py",
      "backend/documents/migrations/0006_documenttypeprofile.py",
      "backend/documents/migrations/__init__.py",
      "backend/environment/migrations/0001_initial.py",
      "backend/environment/migrations/0002_add_owned_resource_mixin.py",
      "backend/environment/migrations/0002_initial.py",
      "backend/environment/migrations/0002_riskrule_riskrulefielddefinition_riskruleexecution.py",
      "backend/environment/migrations/0003_add_business_security_ownership.py",
      "backend/environment/migrations/0004_alter_asset_visibility_and_more.py",
      "backend/environment/migrations/0005_remove_asset_criticality_asset_service_asset_tier.py",
      "backend/environment/migrations/0006_add_composite_indexes.py",
      "backend/environment/migrations/0006_add_performance_indexes.py",
      "backend/environment/migrations/0007_merge_20251104_0914.py",
      "backend/environment/migrations/0008_remove_asset_env_asset_lifecycle_risk_idx_and_more.py",
      "backend/environment/migrations/0009_merge_20251106_2056.py",
      "backend/environment/migrations/0010_remove_asset_environment_owner_t_925258_idx_and_more.py",
      "backend/environment/migrations/0011_asset_idx_asset_type_tier_stat_and_more.py",
      "backend/environment/migrations/0012_add_search_vector_asset.py",
      "backend/environment/migrations/0013_asset_idx_asset_search.py",
      "backend/environment/migrations/0014_asset_managed_by_thirdpartyentity.py",
      "backend/environment/migrations/0015_alter_asset_unique_together_and_more.py",
      "backend/environment/migrations/0017_bitemporal_table_maintenance_tuning.py",
      "backend/environment/migrations/0018_asset_constraints_and_assettechnology_pair_unique.py",
      "backend/environment/migrations/0019_enable_rls.py",
      "backend/environment/migrations/0020_standardize_risk_fields.py",
      "backend/environment/migrations/0021_merge_20251216_1225.py",
      "backend/environment/migrations/0022_rename_env_riskrule_tenant_target_idx_environment_tenant__8ca752_idx_and_more.py",
      "backend/environment/migrations/0023_riskrulefielddefinition_category.py",
      "backend/environment/migrations/0024_add_asset_risk_breakdown.py",
      "backend/environment/migrations/0025_sprint14_asset_enhancements.py",
      "backend/environment/migrations/0026_rename_env_compmap_t_stat_idx_environment_tenant__24f617_idx_and_more.py",
      "backend/environment/migrations/0026_update_asset_search_vector_business_unit_option.py",
      "backend/environment/migrations/0027_merge_20251224_0905.py",
      "backend/environment/migrations/0028_asset_hosting_model_asset_local_service_and_more.py",
      "backend/environment/migrations/0029_asset_data_model_v12_1.py",
      "backend/environment/migrations/0030_remove_asset_idx_asset_category_and_more.py",
      "backend/environment/migrations/0031_risk_rule_library.py",
      "backend/environment/migrations/0032_remove_orgrulevisibility_unique_org_rule_visibility_and_more.py",
      "backend/environment/migrations/0033_add_asset_domain_registration_fields.py",
      "backend/environment/migrations/0034_alter_asset_asset_type.py",
      "backend/environment/migrations/0035_domain_analyzer_integration_sprint16.py",
      "backend/environment/migrations/0036_asset_discovery_tracking_fields.py",
      "backend/environment/migrations/0036_rename_idx_certhistory_asset_environment_tenant__3daa83_idx_and_more.py",
      "backend/environment/migrations/0037_merge_20260109_0700.py",
      "backend/environment/migrations/0038_threat_intelligence_traffic_ranking.py",
      "backend/environment/migrations/0039_remove_asset_idx_asset_threat_malicious_and_more.py",
      "backend/environment/migrations/0040_technology_fingerprinting.py",
      "backend/environment/migrations/0041_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
      "backend/environment/migrations/0042_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
      "backend/environment/migrations/0043_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
      "backend/environment/migrations/0044_asset_discovery_sources.py",
      "backend/environment/migrations/0045_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
      "backend/environment/migrations/0046_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
      "backend/environment/migrations/0047_update_threatintelligencecheck_constraint.py",
      "backend/environment/migrations/0048_backfill_asset_discovery_sources.py",
      "backend/environment/migrations/0049_asset_directory_category.py",
      "backend/environment/migrations/0050_remove_technologycategory_parent_and_more.py",
      "backend/environment/migrations/0051_remove_asset_business_function_id_and_more.py",
      "backend/environment/migrations/__init__.py",
      "backend/environment/permissions.py",
      "backend/environment/risk_rule_library_permissions.py",
      "backend/environment/risk_rule_permissions.py",
      "backend/events/migrations/0001_initial.py",
      "backend/events/migrations/0002_add_owned_resource_mixin.py",
      "backend/events/migrations/0003_add_business_security_ownership.py",
      "backend/events/migrations/0004_alter_event_visibility_and_more.py",
      "backend/events/migrations/0005_incident.py",
      "backend/events/migrations/0006_delete_incident.py",
      "backend/events/migrations/0007_incident_assetentity.py",
      "backend/events/migrations/0008_alter_incident_created_at_alter_incident_created_by_and_more.py",
      "backend/events/migrations/0009_alter_incident_business_owner_team_and_more.py",
      "backend/events/migrations/__init__.py",
      "backend/frameworks/migrations/__init__.py",
      "backend/gcp/deploy.sh",
      "backend/gcp/setup-search-infrastructure.sh",
      "backend/guide-migrations.md",
      "backend/information/migrations/0001_initial.py",
      "backend/information/migrations/0001_initial_ims_models.py",
      "backend/information/migrations/0002_alter_document_content_type_and_more.py",
      "backend/information/migrations/0003_merge_20251106_2056.py",
      "backend/information/migrations/0004_assetentity_fks_and_contenttypes.py",
      "backend/information/migrations/0005_alter_privacyprofile_content_type.py",
      "backend/information/migrations/__init__.py",
      "backend/information/models/migration.py",
      "backend/information/serializers/migration.py",
      "backend/integrations/migrations/0001_initial.py",
      "backend/integrations/migrations/0002_alter_integrationconnection_provider.py",
      "backend/integrations/migrations/0003_slackinstall.py",
      "backend/integrations/migrations/0004_rename_integrations_slack_tenant_team_idx_integration_tenant__f6ace6_idx.py",
      "backend/integrations/migrations/0005_enhance_integrationconnection_for_adapter_pattern.py",
      "backend/integrations/migrations/0006_enhance_integration_connection.py",
      "backend/integrations/migrations/0007_integrationfieldmapping.py",
      "backend/integrations/migrations/0008_scaling_architecture.py",
      "backend/integrations/migrations/0009_alter_integrationprovider_options_and_more.py",
      "backend/integrations/migrations/0010_integrationaction_integrationdatapoint.py",
      "backend/integrations/migrations/0011_seed_google_workspace_actions_complete.py",
      "backend/integrations/migrations/0012_integrationaction_category.py",
      "backend/integrations/migrations/0013_seed_google_workspace_webhooks.py",
      "backend/integrations/migrations/0014_seed_slack_provider_and_actions.py",
      "backend/integrations/migrations/0015_seed_github_provider_and_actions.py",
      "backend/integrations/migrations/0016_add_is_automation_enabled.py",
      "backend/integrations/migrations/0017_integrationaction_integration_auto_en_idx_and_more.py",
      "backend/integrations/migrations/0018_integration_sync_history.py",
      "backend/integrations/migrations/0019_add_sync_history_data_snapshots.py",
      "backend/integrations/migrations/0020_add_sync_type_choices.py",
      "backend/integrations/migrations/0021_rename_nango_connection_id.py",
      "backend/integrations/migrations/0022_normalize_integration_provider_categories.py",
      "backend/integrations/migrations/0023_seed_microsoft_365_provider.py",
      "backend/integrations/migrations/0024_seed_microsoft_teams_provider.py",
      "backend/integrations/migrations/0025_normalize_connected_status_to_active.py",
      "backend/integrations/migrations/0026_seed_google_workspace_provider.py",
      "backend/integrations/migrations/__init__.py",
      "backend/integrations/tests/test_token_lifecycle_guardrails.py",
      "backend/integrations/tests/test_token_refresh.py",
      "backend/integrations/token-lifecycle-standard.md",
      "backend/knowledge/migrations/0001_initial.py",
      "backend/knowledge/migrations/0002_remove_controlmapping_unique_policy_requirement_and_more.py",
      "backend/knowledge/migrations/__init__.py",
      "backend/localization/migrations/0001_initial.py",
      "backend/localization/migrations/0002_add_owned_resource_mixin.py",
      "backend/localization/migrations/0003_add_business_security_ownership.py",
      "backend/localization/migrations/0004_alter_glossaryterm_visibility_and_more.py",
      "backend/localization/migrations/0005_add_analytics_models.py",
      "backend/localization/migrations/0006_alter_translationchangelog_created_by_and_more.py",
      "backend/localization/migrations/0007_translation_ai_config.py",
      "backend/localization/migrations/__init__.py",
      "backend/manual-deploy-with-verify.sh",
      "backend/mapping_intelligence/migrations/0001_initial.py",
      "backend/mapping_intelligence/migrations/0002_add_missing_fields.py",
      "backend/mapping_intelligence/migrations/0002_fielddefinition_mapping_int_synonym_49b140_gin.py",
      "backend/mapping_intelligence/migrations/0003_add_aimachinesettings.py",
      "backend/mapping_intelligence/migrations/0003_fielddefinition_tenant_scope.py",
      "backend/mapping_intelligence/migrations/0003_rename_mapping_int_entity__idx_mapping_int_entity__9ab5a0_idx_and_more.py",
      "backend/mapping_intelligence/migrations/0004_merge_20251020_1449.py",
      "backend/mapping_intelligence/migrations/0005_add_versioning_and_constraints.py",
      "backend/mapping_intelligence/migrations/0007_add_performance_indexes.py",
      "backend/mapping_intelligence/migrations/0008_merge_20251106_2056.py",
      "backend/mapping_intelligence/migrations/0009_mappinghistory_updated_at_mappinghistory_updated_by_and_more.py",
      "backend/mapping_intelligence/migrations/0010_merge_20260105_1105.py",
      "backend/mapping_intelligence/migrations/0011_remove_fielddefinition_mapping_int_entity__030d87_idx_and_more.py",
      "backend/mapping_intelligence/migrations/__init__.py",
      "backend/mapping_intelligence/permissions.py",
      "backend/menu_overrides/migrations/0001_initial.py",
      "backend/menu_overrides/migrations/0002_add_navigation_analytics.py",
      "backend/menu_overrides/migrations/__init__.py",
      "backend/middleware/rbac_enforcement.py",
      "backend/onboarding/migrations/0001_initial.py",
      "backend/onboarding/migrations/0002_onboardingruntimestate.py",
      "backend/onboarding/migrations/__init__.py",
      "backend/operational/migrations/0001_initial.py",
      "backend/operational/migrations/0002_event_sourcing_triggers.py",
      "backend/operational/migrations/0003_fix_event_sourcing_trigger.py",
      "backend/operational/migrations/0004_trigger_request_id.py",
      "backend/operational/migrations/0005_trigger_request_id_metadata.py",
      "backend/operational/migrations/0006_trigger_update_merge_guard.py",
      "backend/operational/migrations/0007_trigger_merge_guard_jsonb.py",
      "backend/operational/migrations/__init__.py",
      "backend/ops/scripts/deploy-celery-jobs.sh",
      "backend/ops/scripts/deploy-rbac-seed.sh",
      "backend/ops/scripts/deploy-rbac-seed.sh",
      "backend/ops/scripts/execute-rbac-seed.sh",
      "backend/ops/scripts/run-migrations.sh",
      "backend/ops/scripts/seed-rbac-permissions.sh",
      "backend/page_actions/migrations/0001_initial.py",
      "backend/page_actions/migrations/0002_remove_customaction_unique_custom_action_per_org_page_and_more.py",
      "backend/page_actions/migrations/0003_standardize_rls_gucs.py",
      "backend/page_actions/migrations/__init__.py",
      "backend/page_actions/permissions.py",
      "backend/page_actions/services/permission_service.py",
      "backend/page_actions/tests/test_permission_service.py",
      "backend/posture/finding_template_library_permissions.py",
      "backend/posture/migrations/0001_initial.py",
      "backend/posture/migrations/0002_add_owned_resource_mixin.py",
      "backend/posture/migrations/0003_add_business_security_ownership.py",
      "backend/posture/migrations/0004_alter_campaign_visibility_alter_finding_visibility_and_more.py",
      "backend/posture/migrations/0005_add_search_vector_finding.py",
      "backend/posture/migrations/0006_finding_idx_finding_search.py",
      "backend/posture/migrations/0007_alter_campaign_scope_assets_alter_finding_asset_and_more.py",
      "backend/posture/migrations/0008_add_finding_likelihood_and_targets.py",
      "backend/posture/migrations/0009_add_finding_template_model.py",
      "backend/posture/migrations/0010_finding_template_library.py",
      "backend/posture/migrations/0011_seed_finding_template_library.py",
      "backend/posture/migrations/0012_rename_posture_ftl_category_status_idx_posture_ftl_cat_status_idx_and_more.py",
      "backend/posture/migrations/__init__.py",
      "backend/run_migrations.py",
      "backend/scripts/audit_rbac_migration.py",
      "backend/scripts/audit_rbac_migration.py",
      "backend/scripts/debug/test_automations_permission_debug.py",
      "backend/scripts/debug/test_rbac_migration.py",
      "backend/scripts/debug/test_rbac_migration.py",
      "backend/setup-auto-deploy.sh",
      "backend/tasks/migrations/0001_initial.py",
      "backend/tasks/migrations/0002_tasklink.py",
      "backend/tasks/migrations/0003_tasksavedview.py",
      "backend/tasks/migrations/0004_task_tags.py",
      "backend/tasks/migrations/0005_task_comments_watchers.py",
      "backend/tasks/migrations/0006_task_attachment.py",
      "backend/tasks/migrations/0007_checklist_item.py",
      "backend/tasks/migrations/0008_alter_checklistitem_created_at_and_more.py",
      "backend/tasks/migrations/0008_task_workflow_sla.py",
      "backend/tasks/migrations/0009_task_provenance.py",
      "backend/tasks/migrations/0010_merge_20251006_1220.py",
      "backend/tasks/migrations/0011_add_owned_resource_mixin.py",
      "backend/tasks/migrations/0016_task_completion_rule.py",
      "backend/tasks/migrations/0017_add_business_security_ownership.py",
      "backend/tasks/migrations/0018_alter_checklistitem_visibility_alter_task_visibility_and_more.py",
      "backend/tasks/migrations/0019_add_search_vector_task.py",
      "backend/tasks/migrations/0020_task_idx_task_search.py",
      "backend/tasks/migrations/0021_enable_rls.py",
      "backend/tasks/migrations/0022_add_task_decisions.py",
      "backend/tasks/migrations/0023_convert_task_decision_to_task_type.py",
      "backend/tasks/migrations/0024_enforce_single_pending_task_decision.py",
      "backend/tasks/migrations/0025_alter_task_status_alter_task_type.py",
      "backend/tasks/migrations/__init__.py",
      "backend/tests/admin/test_admin_notifications_policy_rbac.py",
      "backend/tests/admin/test_admin_permission_audit.py",
      "backend/tests/admin/test_admin_permission_audit_correlation.py",
      "backend/tests/admin/test_admin_roles_rbac_edit_allow.py",
      "backend/tests/admin/test_admin_roles_rbac_edit_deny.py",
      "backend/tests/admin/test_admin_users_rbac_allow.py",
      "backend/tests/audit/test_audit_events_rbac_deny_audit.py",
      "backend/tests/audit/test_audit_export_rbac_deny.py",
      "backend/tests/audit/test_audit_export_rbac_superuser.py",
      "backend/tests/audit/test_audit_rbac.py",
      "backend/tests/audit/test_audit_rbac_endpoints.py",
      "backend/tests/audit/test_audit_registry_billing_vendor_required.py",
      "backend/tests/audit/test_audit_registry_permissions_required.py",
      "backend/tests/critical/test_auth_oidc.py",
      "backend/tests/integration/test_auth0_idp_asset_bootstrap.py",
      "backend/tests/integration/test_collaboration_authorization.py",
      "backend/tests/integration/test_db_viewer_rbac_allow.py",
      "backend/tests/integration/test_schema_permissions.py",
      "backend/tests/integration/test_schema_ui_permissions.py",
      "backend/tests/integration/test_secret_hashing.py",
      "backend/tests/integration/test_teams_rbac.py",
      "backend/tests/integration/test_teams_rbac_allow.py",
      "backend/tests/integration/test_thirdparty_authorization.py",
      "backend/tests/integration/test_thirdparty_relationship_authorization.py",
      "backend/tests/security/test_auth_flows_comprehensive.py",
      "backend/tests/security/test_auth_login_ratelimit.py",
      "backend/tests/security/test_auth_logout_csrf.py",
      "backend/tests/security/test_auth_session.py",
      "backend/tests/security/test_impersonation_rbac.py",
      "backend/tests/security/test_rbac_admin_api.py",
      "backend/tests/security/test_rbac_casl_mapping.py",
      "backend/tests/security/test_rbac_forbidden_json.py",
      "backend/tests/security/test_rbac_forbidden_json_shape.py",
      "backend/tests/security/test_rbac_risk_matrix.py",
      "backend/tests/security/test_rbac_risk_recalc_command.py",
      "backend/tests/security/test_rbac_settings_guard.py",
      "backend/tests/security/test_thirdparty_unauth_endpoints.py",
      "backend/tests/suppliers/test_suppliers_reports_rbac.py",
      "backend/thirdparties/migrations/0001_initial.py",
      "backend/thirdparties/migrations/0002_enable_rls_policies.py",
      "backend/thirdparties/migrations/0003_bitemporal_constraints.py",
      "backend/thirdparties/migrations/0004_rename_tables_suppliers_to_thirdparties.py",
      "backend/thirdparties/migrations/0005_rename_thirdparties_a_tenant__c63003_idx_thirdpartie_tenant__0c77fd_idx_and_more.py",
      "backend/thirdparties/migrations/0006_rename_thirdparties_a_tenant__c63003_idx_thirdpartie_tenant__0c77fd_idx_and_more.py",
      "backend/thirdparties/migrations/0007_add_asset_service_offering.py",
      "backend/thirdparties/migrations/0008_rename_thirdparties_a_tenant__c63003_idx_thirdpartie_tenant__0c77fd_idx_and_more.py",
      "backend/thirdparties/migrations/0009_supplier_graph_view.py",
      "backend/thirdparties/migrations/0010_add_missing_fields.py",
      "backend/thirdparties/migrations/0011_enable_rls.py",
      "backend/thirdparties/migrations/0012_remove_thirdpartyrelationship_thirdparty_rel_unique_and_more.py",
      "backend/thirdparties/migrations/0012_suppliers_saved_view.py",
      "backend/thirdparties/migrations/0013_asset_service_offering_constraints.py",
      "backend/thirdparties/migrations/0014_check_constraints.py",
      "backend/thirdparties/migrations/0015_performance_indexes.py",
      "backend/thirdparties/migrations/0016_partial_indexes.py",
      "backend/thirdparties/migrations/0017_add_frontend_aligned_fields.py",
      "backend/thirdparties/migrations/0018_add_gdpr_data_privacy_models.py",
      "backend/thirdparties/migrations/0019_remove_dataprivacycontact_created_by_and_more.py",
      "backend/thirdparties/migrations/0020_add_privacy_enhancements.py",
      "backend/thirdparties/migrations/0021_remove_dataprivacycontact_created_by_and_more.py",
      "backend/thirdparties/migrations/0022_repair_privacy_columns.py",
      "backend/thirdparties/migrations/0023_remove_asset_asset_tenant_type_idx_and_more.py",
      "backend/thirdparties/migrations/0024_remove_asset_asset_tenant_type_idx_and_more.py",
      "backend/thirdparties/migrations/0025_alter_document_content_type_and_more.py",
      "backend/thirdparties/migrations/0026_supplierassessment_supplierchangerequest_and_more.py",
      "backend/thirdparties/migrations/0027_dataprivacycontact_dataprivacyprofile_and_more.py",
      "backend/thirdparties/migrations/0028_merge_20251023_1923.py",
      "backend/thirdparties/migrations/0029_tprm_policy_owner_team_doc_source.py",
      "backend/thirdparties/migrations/0030_rename_tp_tenant_owner_user_idx_thirdpartie_tenant__4ef8e4_idx_and_more.py",
      "backend/thirdparties/migrations/0031_add_business_security_ownership.py",
      "backend/thirdparties/migrations/0032_dataprivacycontact_dataprivacyprofile_and_more.py",
      "backend/thirdparties/migrations/0033_alter_thirdparty_visibility.py",
      "backend/thirdparties/migrations/0034_alter_thirdparty_relationship_types.py",
      "backend/thirdparties/migrations/0035_thirdparty_frameworks_alter_thirdparty_tags.py",
      "backend/thirdparties/migrations/0036_alter_thirdparty_tags.py",
      "backend/thirdparties/migrations/0037_add_composite_indexes.py",
      "backend/thirdparties/migrations/0037_add_performance_indexes.py",
      "backend/thirdparties/migrations/0038_merge_20251104_0914.py",
      "backend/thirdparties/migrations/0039_remove_directorylinkconfig_tp_link_sync_idx_and_more.py",
      "backend/thirdparties/migrations/0041_search_extensions_and_indexes.py",
      "backend/thirdparties/migrations/0042_add_search_vector_thirdparty.py",
      "backend/thirdparties/migrations/0043_thirdparty_idx_thirdparty_search.py",
      "backend/thirdparties/migrations/0044_thirdparty_entity_versioning.py",
      "backend/thirdparties/migrations/0045_dataprivacyprofile_third_party_entity.py",
      "backend/thirdparties/migrations/0046_rename_thirdparty_tenant_entity_idx_thirdpartie_tenant__205199_idx_and_more.py",
      "backend/thirdparties/migrations/0047_standardize_rls_gucs.py",
      "backend/thirdparties/migrations/0048_alter_directorylinkconfig_linked_legal_entity_and_more.py",
      "backend/thirdparties/migrations/0049_fix_thirdparty_no_overlap_valid_to_infinity.py",
      "backend/thirdparties/migrations/0050_bitemporal_table_maintenance_tuning.py",
      "backend/thirdparties/migrations/0051_standardize_risk_fields.py",
      "backend/thirdparties/migrations/0052_alter_thirdparty_risk_factors_and_more.py",
      "backend/thirdparties/migrations/0053_directorylinkconfig_linked_local_provider.py",
      "backend/thirdparties/migrations/0054_alter_thirdparty_lifecycle_status.py",
      "backend/thirdparties/migrations/0055_thirdparty_adn_parity_fields.py",
      "backend/thirdparties/migrations/0056_functionalrole_industrycodecrosswalk_and_more.py",
      "backend/thirdparties/migrations/0057_seed_functional_roles.py",
      "backend/thirdparties/migrations/0058_seed_industry_crosswalk.py",
      "backend/thirdparties/migrations/0059_thirdparty_adn_parity_fields.py",
      "backend/thirdparties/migrations/0060_supplier_directory_category.py",
      "backend/thirdparties/migrations/0061_thirdparty_control_frameworks.py",
      "backend/thirdparties/migrations/0062_migrate_frameworks_m2m.py",
      "backend/thirdparties/migrations/0063_alter_thirdparty_frameworks.py",
      "backend/thirdparties/migrations/FOLDER.migrations.md",
      "backend/thirdparties/migrations/__init__.py",
      "backend/webhooks/migrations/0001_initial.py",
      "backend/webhooks/migrations/0002_rename_webhooks_de_subscri_f5d8c1_idx_webhooks_de_subscri_f97236_idx_and_more.py",
      "backend/webhooks/migrations/0003_unique_constraints.py",
      "backend/webhooks/migrations/0004_add_owned_resource_mixin.py",
      "backend/webhooks/migrations/0005_add_business_security_ownership.py",
      "backend/webhooks/migrations/0006_alter_webhookdelivery_visibility_and_more.py",
      "backend/webhooks/migrations/0007_hash_webhook_secrets.py",
      "backend/webhooks/migrations/0007_hash_webhook_secrets.py",
      "backend/webhooks/migrations/0008_alter_webhooksubscription_secret.py",
      "backend/webhooks/migrations/0008_alter_webhooksubscription_secret.py",
      "backend/webhooks/migrations/__init__.py",
      "contracts/migration-to-schemathesis.md",
      "devops/grafana/dashboards/rbac-dashboard.json",
      "devops/prometheus/rules/rbac-alerts.yml",
      "docs/access-auth.md",
      "docs/agents/context/admin.billing.md",
      "docs/api/rbac-api-reference.md",
      "docs/api/rbac-api.md",
      "docs/api/rbac-openapi.json",
      "docs/api/rbac-quick-reference.md",
      "docs/architecture/architecture-deployment.md",
      "docs/architecture/rbac-architecture.md",
      "docs/architecture/security-audit-rbac.md",
      "docs/design-system/automated-deployment-setup.md",
      "docs/design-system/design-tokens-tier-guide.md",
      "docs/feature-flags/catalog-key-migration.md",
      "docs/feature-specs/admin/page-actions/09-deployment-guide.md",
      "docs/feature-specs/controls/deployment-checklist.md",
      "docs/feature-specs/information/my-environment-rbac-integration.md",
      "docs/feature-specs/rbac/admin-roles-review-and-cleanup.md",
      "docs/feature-specs/rbac/rbac-spec.md",
      "docs/feature-specs/search-deployment-guide.md",
      "docs/guides/authentication-setup.md",
      "docs/guides/cost-optimized-deployment.md",
      "docs/guides/deployment-guide-permissions.md",
      "docs/guides/deployment-guide-permissions.md",
      "docs/guides/multi-tenant-deployment-critical.md",
      "docs/guides/post-deployment-setup.md",
      "docs/guides/post-deployment-verification.md",
      "docs/guides/rbac-admin-guide.md",
      "docs/permissions.md",
      "docs/plans/infrastructure-options-comparison.md",
      "docs/prd/rbac-simplified-design.md",
      "docs/rbac-cache-implementation.md",
      "docs/reference/rbac.yaml",
      "docs/reference/reference-rbac-permission-sync.md",
      "docs/runbooks/deploy-admin.md",
      "docs/runbooks/deployment-checklist.md",
      "docs/runbooks/rbac-operations-runbook.md",
      "docs/runbooks/rbac-risk-policy.md",
      "docs/runbooks/runbook-deployment-best-practices.md",
      "docs/runbooks/runbook-production-deployment.md",
      "docs/secret-management-plan.md",
      "docs/security/rbac-risk-policy.md",
      "e2e/fixtures/auth.fixture.ts",
      "e2e/page-objects/auth/login.page.ts",
      "e2e/tests/auth/authentication.spec.ts",
      "manual-deployment-steps.md",
      "migration-complete.md",
      "migration-status.md",
      "migrations-applied-success.md",
      "packages/app-shared/src/app/AuthenticatedApp.tsx",
      "packages/app-shared/src/auth/AbilityContext.shared.ts",
      "packages/app-shared/src/auth/AbilityProvider.ts",
      "packages/app-shared/src/auth/AbilityProviderRoot.tsx",
      "packages/app-shared/src/auth/AuthError.tsx",
      "packages/app-shared/src/auth/FOLDER.auth.md",
      "packages/app-shared/src/auth/NoTenantAccess.tsx",
      "packages/app-shared/src/auth/SessionExpiryWarningProvider.tsx",
      "packages/app-shared/src/auth/SessionGate.tsx",
      "packages/app-shared/src/auth/UseEnvironmentPermissions.tsx",
      "packages/app-shared/src/auth/UseEnvironmentPermissions.tsx",
      "packages/app-shared/src/auth/__tests__/FOLDER.__tests__.md",
      "packages/app-shared/src/auth/__tests__/permissionGrouping.test.ts",
      "packages/app-shared/src/auth/__tests__/permissionGrouping.test.ts",
      "packages/app-shared/src/auth/ability.ts",
      "packages/app-shared/src/auth/can.ts",
      "packages/app-shared/src/auth/logoutBroadcast.ts",
      "packages/app-shared/src/auth/logoutClient.ts",
      "packages/app-shared/src/auth/permissionGrouping.ts",
      "packages/app-shared/src/auth/permissionGrouping.ts",
      "packages/app-shared/src/auth/rbac-canonical.ts",
      "packages/app-shared/src/auth/rbac-canonical.ts",
      "packages/app-shared/src/auth/session.ts",
      "packages/app-shared/src/auth/sessionExpiryWarningContext.ts",
      "packages/app-shared/src/auth/useSessionHeartbeat.ts",
      "packages/app-shared/src/components/admin/AdminBillingView.tsx",
      "packages/app-shared/src/components/admin/OrgBillingOverviewView.tsx",
      "packages/app-shared/src/components/admin/TenantBillingTab.tsx",
      "packages/app-shared/src/components/admin/roles/BatchPermissionUpdates.tsx",
      "packages/app-shared/src/components/admin/roles/PermissionConflictDetector.tsx",
      "packages/app-shared/src/components/admin/roles/PermissionMatrix.tsx",
      "packages/app-shared/src/components/admin/roles/PermissionMatrixSkeleton.tsx",
      "packages/app-shared/src/components/admin/roles/PermissionsList.tsx",
      "packages/app-shared/src/components/admin/roles/__tests__/BatchPermissionUpdates.test.tsx",
      "packages/app-shared/src/components/auth/PermissionDenied.tsx",
      "packages/app-shared/src/components/auth/PermissionDenied.tsx",
      "packages/app-shared/src/constants/rbac-module-settings.md",
      "packages/app-shared/src/constants/rbac.ts",
      "packages/app-shared/src/features/admin/components/AdminBillingView.tsx",
      "packages/app-shared/src/features/admin/components/roles/PermissionConflictDetector.tsx",
      "packages/app-shared/src/features/admin/components/roles/PermissionMatrix.tsx",
      "packages/app-shared/src/features/admin/components/roles/PermissionMatrixSkeleton.tsx",
      "packages/app-shared/src/features/admin/components/roles/permissionConflictRules.ts",
      "packages/app-shared/src/features/admin/components/roles/permissionMatrix.shared.ts",
      "packages/app-shared/src/features/admin/hooks/useUsersAndPermissions.ts",
      "packages/app-shared/src/features/admin/pages/AdminBillingPage.tsx",
      "packages/app-shared/src/features/admin/pages/AdminMigrationDashboardPage.tsx",
      "packages/app-shared/src/features/admin/pages/AdminMigrationDashboardPageView.tsx",
      "packages/app-shared/src/features/auth/components/PermissionDenied.tsx",
      "packages/app-shared/src/features/auth/components/PermissionDenied.tsx",
      "packages/app-shared/src/features/auth/components/index.ts",
      "packages/app-shared/src/features/auth/index.ts",
      "packages/app-shared/src/features/auth/utils/ability.ts",
      "packages/app-shared/src/features/auth/utils/can.ts",
      "packages/app-shared/src/features/auth/utils/index.ts",
      "packages/app-shared/src/features/auth/utils/permissionGrouping.ts",
      "packages/app-shared/src/features/auth/utils/permissionGrouping.ts",
      "packages/app-shared/src/features/auth/utils/session.ts",
      "packages/app-shared/src/features/information/hooks/useMigration.ts",
      "packages/app-shared/src/features/information/pages/MigrationConflictsPage.tsx",
      "packages/app-shared/src/features/information/pages/MigrationDashboardPage.tsx",
      "packages/app-shared/src/features/information/pages/MigrationImportPage.tsx",
      "packages/app-shared/src/features/org/pages/OrgBillingPage.tsx",
      "packages/app-shared/src/hooks/admin/usePermissionsCatalog.ts",
      "packages/app-shared/src/hooks/admin/useUsersAndPermissions.ts",
      "packages/app-shared/src/hooks/information/useMigration.ts",
      "packages/app-shared/src/hooks/lib/parsePermissions.ts",
      "packages/app-shared/src/hooks/permissions/__tests__/useCanAccess.test.tsx",
      "packages/app-shared/src/hooks/permissions/__tests__/usePermission.test.tsx",
      "packages/app-shared/src/hooks/permissions/index.ts",
      "packages/app-shared/src/hooks/permissions/testUtils.tsx",
      "packages/app-shared/src/hooks/permissions/useAbility.ts",
      "packages/app-shared/src/hooks/permissions/useCanAccess.ts",
      "packages/app-shared/src/hooks/permissions/usePermission.ts",
      "packages/app-shared/src/hooks/useOrgBillingApi.ts",
      "packages/app-shared/src/lib/__tests__/permissions.test.ts",
      "packages/app-shared/src/lib/permissions.ts",
      "packages/app-shared/src/lib/personalTokensApi.ts",
      "packages/app-shared/src/pages/AuthLogoutPage.tsx",
      "packages/app-shared/src/pages/platform/AuthAnalyticsPage.impl.tsx",
      "packages/app-shared/src/pages/platform/AuthAnalyticsPage.tsx",
      "packages/app-shared/src/preauth/debug.ts",
      "packages/app-shared/src/preauth/index.ts",
      "packages/app-shared/src/preauth/network.ts",
      "packages/app-shared/src/preauth/session.ts",
      "packages/app-shared/src/preauth/telemetry.ts",
      "packages/app-shared/src/preauth/theme.ts",
      "packages/app-shared/src/preauth/types.ts",
      "packages/app-shared/src/preauth/ui.ts",
      "packages/app-shared/src/preauth/utils.test.ts",
      "packages/app-shared/src/preauth/utils.ts",
      "packages/app-shared/src/router/Unauthorized.tsx",
      "packages/app-shared/src/tests/admin.billing.a11y.test.tsx",
      "packages/app-shared/src/tests/admin.billing.exportmenu.test.tsx",
      "packages/app-shared/src/tests/admin.billing.mobile.test.tsx",
      "packages/app-shared/src/tests/admin.billing.toolbar.smoke.test.tsx",
      "packages/app-shared/src/tests/admin.users.rbac.banner.test.tsx",
      "packages/app-shared/src/tests/api.credentials.test.ts",
      "packages/app-shared/src/tests/auth.can.test.ts",
      "packages/app-shared/src/tests/permission.gate.test.tsx",
      "packages/app-shared/src/tests/router.unauthorized.ui.test.tsx",
      "packages/app-shared/src/tests/suppliers.directory.views.rbac.test.tsx",
      "packages/app-shared/src/types/rbac.ts",
      "packages/auth/package.json",
      "packages/auth/src/__tests__/permissionGrouping.test.ts",
      "packages/auth/src/__tests__/permissionGrouping.test.ts",
      "packages/auth/src/__tests__/rbac-canonical.test.ts",
      "packages/auth/src/__tests__/rbac-canonical.test.ts",
      "packages/auth/src/ability.ts",
      "packages/auth/src/can.ts",
      "packages/auth/src/index.ts",
      "packages/auth/src/logout/broadcast.ts",
      "packages/auth/src/logout/client.ts",
      "packages/auth/src/logout/index.ts",
      "packages/auth/src/permissionGrouping.ts",
      "packages/auth/src/permissionGrouping.ts",
      "packages/auth/src/rbac-canonical.ts",
      "packages/auth/src/rbac-canonical.ts",
      "packages/auth/src/session.ts",
      "packages/auth/test-results/junit.xml",
      "packages/auth/tsconfig.json",
      "packages/auth/tsconfig.tsbuildinfo",
      "packages/documentation/migration/page-checklist.json",
      "packages/types/src/rbac.ts",
      "packages/ui/.ai/design-tokens.json",
      "packages/ui/.ai/migration-rules.json",
      "packages/ui/src/components/molecules/TokenPicker/TokenPicker.tsx",
      "packages/ui/src/components/molecules/TokenPicker/index.ts",
      "packages/ui/src/tokens/components.css",
      "packages/ui/src/tokens/index.css",
      "packages/ui/src/tokens/index.ts",
      "packages/ui/src/tokens/primitives.css",
      "packages/ui/src/tokens/semantic.css",
      "packages/ui/src/tokens/themes/dark.css",
      "postgres-18-migration-guide.md",
      "rbac-cache-delivery.md",
      "rbac-cache-quickstart.md",
      "scripts/checks/check-customer-preauth-no-design-system.mjs",
      "scripts/ci/check-endpoint-permissions.mjs",
      "scripts/ci/check-permission-metadata.mjs",
      "scripts/ci/check-route-permissions.sh",
      "scripts/ci/check_migrations.sh",
      "scripts/ci/validate-rbac-sync.mjs",
      "scripts/deploy-types.cjs",
      "scripts/deployment/build-production.sh",
      "scripts/design-system/generate-token-json.mjs",
      "scripts/migration/audit-page-components.mjs",
      "scripts/validate-deployment.sh",
      "scripts/validation/validate_permissions.py",
      "scripts/verify-phase0-deployment.sh",
      "scripts/verify_migration.sh",
      "tests/contract/consumers/auth.contract.test.ts",
      "tools/mcp-mordor/src/tools/rbac.ts"
    ]
  },
  "commands": [
    "cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli task anchors --repo '/Users/christophehenner/Downloads/Repositories/Aethyme Playground' --task <task> --json-output",
    "cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli task scope --repo '/Users/christophehenner/Downloads/Repositories/Aethyme Playground' --task <task> --json-output",
    "cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph configs '/Users/christophehenner/Downloads/Repositories/Aethyme Playground' packages --json-output",
    "cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph expand '/Users/christophehenner/Downloads/Repositories/Aethyme Playground' <anchor-id> --json-output"
  ]
}
```

<!-- Signal-to-Noise Assessment
Rate the relevance of the navigation context provided to the leverage condition:
- Anchors: were the starting points useful?
- Scope: did in-scope files cover what the agent needed?
- Navigation order: was the reading order helpful?
- Noise: what was included but not needed?
-->
## Comparison

| Metric | Control | Explore | Leverage |
| --- | --- | --- | --- |
| Prompt chars | `322` | `1633` | `257` |

- Navigation items surfaced: `1`
- Risk items surfaced: `961`

## Reference

### Output Schema

```json
{
  "type": "object",
  "additionalProperties": false,
  "required": [
    "config_target",
    "code_target",
    "management_area",
    "relationship_chain",
    "rejected_candidates",
    "confidence"
  ],
  "properties": {
    "config_target": {
      "type": "object",
      "additionalProperties": false,
      "required": [
        "path",
        "why"
      ],
      "properties": {
        "path": {
          "type": "string"
        },
        "why": {
          "type": "string"
        }
      }
    },
    "code_target": {
      "type": "object",
      "additionalProperties": false,
      "required": [
        "path",
        "why"
      ],
      "properties": {
        "path": {
          "type": "string"
        },
        "why": {
          "type": "string"
        }
      }
    },
    "management_area": {
      "type": "object",
      "additionalProperties": false,
      "required": [
        "name",
        "why"
      ],
      "properties": {
        "name": {
          "type": "string"
        },
        "why": {
          "type": "string"
        }
      }
    },
    "relationship_chain": {
      "type": "array",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": [
          "from",
          "to",
          "relation"
        ],
        "properties": {
          "from": {
            "type": "string"
          },
          "to": {
            "type": "string"
          },
          "relation": {
            "type": "string"
          }
        }
      }
    },
    "rejected_candidates": {
      "type": "array",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": [
          "path",
          "reason"
        ],
        "properties": {
          "path": {
            "type": "string"
          },
          "reason": {
            "type": "string"
          }
        }
      }
    },
    "confidence": {
      "type": "string"
    }
  }
}
```

### Scoring Rubric

```json
{
  "weights": {
    "config_target": 30,
    "code_target": 30,
    "management_area": 20,
    "relationship_chain": 20
  },
  "notes": [
    "Exact config/code path matches carry most of the score.",
    "Relationship chain must express both ownership and management links."
  ]
}
```

### Reference Output

```json
{
  "config_target": {
    "path": "packages/auth/package.json",
    "why": "manifest/config linked to the runtime entrypoint"
  },
  "code_target": {
    "path": "packages/ui/src/tokens/index.ts",
    "why": "entrypoint file linked by the configuration graph"
  },
  "management_area": {
    "name": "packages",
    "why": "top-level area linked by the configuration graph"
  },
  "relationship_chain": [
    {
      "from": "packages/auth/package.json",
      "to": "packages",
      "relation": "configures"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/api/mapping-intelligence/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/auth/ability.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/auth/can.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/auth/permissionGrouping.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/auth/rbac-canonical.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/auth/session.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/admin/adn/local-entries/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/admin/adn/taxonomy/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/admin/db/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/adn/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/adn/widgets/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/chrome/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/chrome/layout/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/collaboration/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/domain/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/information/ComplianceStudio/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/information/PolicyStudio/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/information/shared/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/integrity/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/layout/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/mapping-intelligence/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/suppliers/assessments/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/suppliers/changes/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/suppliers/comments/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/suppliers/exceptions/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/suppliers/incidents/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/suppliers/signals/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/config/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/config/page-actions/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/admin/components/analytics/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/admin/components/assignments/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/admin/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/admin/components/integrations/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/admin/components/menu-manager/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/admin/components/risk-rules/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/admin/components/users/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/admin/hooks/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/admin/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/admin/pages/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/admin/schemas/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/admin/types/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/auth/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/auth/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/auth/utils/ability.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/auth/utils/can.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/auth/utils/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/auth/utils/permissionGrouping.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/auth/utils/session.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/automations/components/builder-v2/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/automations/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/automations/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/automations/pages/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/bulk/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/campaigns/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/campaigns/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/campaigns/pages/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/collaboration/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/collaboration/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/collaboration/pages/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/controls/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/controls/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/controls/pages/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/controls/schemas/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/controls/types/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/environment/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/environment/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/environment/pages/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/environment/schemas/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/information/components/ComplianceStudio/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/information/components/PolicyStudio/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/information/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/information/components/shared/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/information/hooks/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/information/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/information/pages/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/notifications/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/notifications/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/notifications/pages/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/notifications/schemas/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/notifications/types/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/onboarding2/__stories__/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/onboarding2/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/onboarding2/components/steps/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/onboarding2/hooks/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/onboarding2/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/org/pages/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/platform/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/platform/pages/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/policy/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/policy/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/profile/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/profile/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/profile/pages/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/reporting/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/reporting/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/reporting/schemas/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/risk/components/builder/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/risk/components/builder/inspectors/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/risk/components/builder/nodes/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/risk/components/calculator/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/risk/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/risk/components/manager/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/risk/components/shared/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/risk/hooks/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/risk/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/risk/pages/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/risk/services/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/risk/types/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/risk/utils/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/suppliers/components/assessments/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/suppliers/components/changes/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/suppliers/components/comments/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/suppliers/components/exceptions/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/suppliers/components/incidents/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/suppliers/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/suppliers/components/signals/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/suppliers/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/suppliers/pages/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/suppliers/schemas/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/suppliers/types/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/tasks/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/tasks/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/hooks/admin/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/hooks/chrome/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/hooks/collaboration/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/hooks/environment/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/hooks/information/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/hooks/menu/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/hooks/permissions/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/lib/api/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/lib/automations/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/lib/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/preauth/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/preauth/session.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/providers/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/schemas/api/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/schemas/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/shared/hooks/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/shared/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/stores/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/types/api/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/types/automations/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/types/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/auth/src/ability.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/auth/src/can.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/auth/src/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/auth/src/logout/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/auth/src/permissionGrouping.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/auth/src/rbac-canonical.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/auth/src/session.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/config/src/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/eslint-plugin-aeptus/src/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/types/src/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/types/src/session.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/Avatar/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/BackButton/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/Badge/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/Button/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/Card/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/Checkbox/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/DataSourceBadge/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/Divider/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/DropZone/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/HelpTrigger/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/Icon/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/IconButton/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/Input/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/LazyImage/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/Logo/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/ManagedChip/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/NumericStepper/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/OptionList/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/PhoneInput/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/ScopeRiskBadge/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/ScrollArea/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/SemanticBadge/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/Skeleton/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/Spinner/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/StatusIndicator/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/Switch/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/Table/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/Textarea/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/TierBadge/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/ToolbarSelect/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/Tooltip/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/charts/AreaChart/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/charts/BarChart/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/charts/ChartCard/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/charts/KpiWidget/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/charts/LineChart/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/charts/PieChart/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/charts/Sparkline/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Accordion/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/ActionBar/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/ActionForm/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Alert/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/AlertDialog/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/AsyncCombobox/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/AvatarUpload/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Breadcrumbs/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/CodeSnippet/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Collapsible/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/ColorPicker/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Combobox/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/ComboboxWithRefs/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Command/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/ConfirmDialog/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/DatePicker/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/DateRangePicker/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/DateTimePicker/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/DescriptionList/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/DialogSection/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Drawer/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/DropdownMenu/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/EmptyState/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/FieldWrapper/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/FloatingPanel/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/InlineEdit/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Input/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Label/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Modal/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/MultiSelect/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/MultiTypeCombobox/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/OverlayWidget/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/OwnerSelectBadge/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Pagination/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Popover/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/ProgressBar/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/PullToRefresh/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/RHFForm/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/RadioGroup/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/RangeSlider/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/SavedViews/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/SearchInput/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Section/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/SegmentedControl/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Select/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/SensitiveValueDisplay/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/ServerPagination/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Sheet/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Skeleton/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Skeleton/variants/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Slider/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/StatusBanner/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Stepper/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Switch/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/TableSortHeader/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Tabs/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Toast/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/ToastContainer/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/TokenPicker/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Toolbar/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Tooltip/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/ValidationSummary/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/ViewToggle/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/organisms/Carousel/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/organisms/ColumnVisibilityMenu/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/organisms/CommandPalette/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/organisms/DataTable/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/organisms/FileUpload/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/organisms/Form/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/organisms/RichTextEditor/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/organisms/Sidebar/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/BulkActionsToolbar/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/FilterBuilder/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/IntegrationCard/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/ActionBar/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/ActivityFeed/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/BulkEditDialog/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/CardFilter/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/CollectionView/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/EnhancedSearchBar/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/ExportDialog/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/FacetFilter/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/FilterBar/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/FilterPanel/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/KanbanBoard/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/LogicBuilder/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/ModalFilter/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/NotificationCenter/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/RangeSliderFilter/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/Timeline/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/ToggleFilter/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/UserMenu/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/flows/OnboardingTooltip/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/flows/OnboardingTooltips/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/flows/Wizard/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/templates/DashboardLayout/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/templates/DetailPage/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/templates/FormPage/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/templates/ListPage/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/templates/MasterDetail/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/templates/ResourceLayout/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/templates/SettingsPage/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/templates/WizardPage/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/utilities/VirtualizedTableBody/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/utilities/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/hooks/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/tokens/index.ts",
      "relation": "entrypoint_for"
    }
  ],
  "rejected_candidates": [],
  "confidence": "high"
}
```

### Challenge

```json
{
  "kind": "navigation_ctf",
  "task": "Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.",
  "reference_output": {
    "config_target": {
      "path": "packages/auth/package.json",
      "why": "manifest/config linked to the runtime entrypoint"
    },
    "code_target": {
      "path": "packages/ui/src/tokens/index.ts",
      "why": "entrypoint file linked by the configuration graph"
    },
    "management_area": {
      "name": "packages",
      "why": "top-level area linked by the configuration graph"
    },
    "relationship_chain": [
      {
        "from": "packages/auth/package.json",
        "to": "packages",
        "relation": "configures"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/api/mapping-intelligence/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/auth/ability.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/auth/can.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/auth/permissionGrouping.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/auth/rbac-canonical.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/auth/session.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/admin/adn/local-entries/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/admin/adn/taxonomy/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/admin/db/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/adn/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/adn/widgets/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/chrome/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/chrome/layout/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/collaboration/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/domain/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/information/ComplianceStudio/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/information/PolicyStudio/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/information/shared/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/integrity/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/layout/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/mapping-intelligence/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/suppliers/assessments/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/suppliers/changes/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/suppliers/comments/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/suppliers/exceptions/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/suppliers/incidents/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/suppliers/signals/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/config/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/config/page-actions/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/admin/components/analytics/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/admin/components/assignments/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/admin/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/admin/components/integrations/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/admin/components/menu-manager/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/admin/components/risk-rules/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/admin/components/users/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/admin/hooks/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/admin/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/admin/pages/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/admin/schemas/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/admin/types/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/auth/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/auth/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/auth/utils/ability.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/auth/utils/can.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/auth/utils/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/auth/utils/permissionGrouping.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/auth/utils/session.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/automations/components/builder-v2/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/automations/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/automations/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/automations/pages/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/bulk/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/campaigns/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/campaigns/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/campaigns/pages/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/collaboration/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/collaboration/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/collaboration/pages/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/controls/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/controls/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/controls/pages/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/controls/schemas/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/controls/types/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/environment/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/environment/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/environment/pages/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/environment/schemas/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/information/components/ComplianceStudio/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/information/components/PolicyStudio/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/information/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/information/components/shared/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/information/hooks/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/information/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/information/pages/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/notifications/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/notifications/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/notifications/pages/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/notifications/schemas/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/notifications/types/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/onboarding2/__stories__/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/onboarding2/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/onboarding2/components/steps/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/onboarding2/hooks/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/onboarding2/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/org/pages/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/platform/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/platform/pages/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/policy/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/policy/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/profile/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/profile/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/profile/pages/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/reporting/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/reporting/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/reporting/schemas/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/risk/components/builder/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/risk/components/builder/inspectors/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/risk/components/builder/nodes/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/risk/components/calculator/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/risk/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/risk/components/manager/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/risk/components/shared/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/risk/hooks/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/risk/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/risk/pages/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/risk/services/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/risk/types/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/risk/utils/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/suppliers/components/assessments/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/suppliers/components/changes/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/suppliers/components/comments/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/suppliers/components/exceptions/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/suppliers/components/incidents/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/suppliers/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/suppliers/components/signals/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/suppliers/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/suppliers/pages/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/suppliers/schemas/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/suppliers/types/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/tasks/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/tasks/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/hooks/admin/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/hooks/chrome/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/hooks/collaboration/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/hooks/environment/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/hooks/information/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/hooks/menu/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/hooks/permissions/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/lib/api/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/lib/automations/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/lib/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/preauth/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/preauth/session.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/providers/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/schemas/api/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/schemas/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/shared/hooks/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/shared/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/stores/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/types/api/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/types/automations/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/types/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/auth/src/ability.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/auth/src/can.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/auth/src/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/auth/src/logout/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/auth/src/permissionGrouping.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/auth/src/rbac-canonical.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/auth/src/session.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/config/src/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/eslint-plugin-aeptus/src/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/types/src/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/types/src/session.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/Avatar/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/BackButton/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/Badge/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/Button/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/Card/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/Checkbox/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/DataSourceBadge/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/Divider/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/DropZone/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/HelpTrigger/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/Icon/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/IconButton/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/Input/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/LazyImage/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/Logo/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/ManagedChip/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/NumericStepper/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/OptionList/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/PhoneInput/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/ScopeRiskBadge/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/ScrollArea/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/SemanticBadge/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/Skeleton/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/Spinner/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/StatusIndicator/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/Switch/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/Table/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/Textarea/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/TierBadge/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/ToolbarSelect/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/Tooltip/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/charts/AreaChart/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/charts/BarChart/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/charts/ChartCard/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/charts/KpiWidget/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/charts/LineChart/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/charts/PieChart/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/charts/Sparkline/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Accordion/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/ActionBar/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/ActionForm/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Alert/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/AlertDialog/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/AsyncCombobox/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/AvatarUpload/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Breadcrumbs/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/CodeSnippet/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Collapsible/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/ColorPicker/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Combobox/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/ComboboxWithRefs/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Command/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/ConfirmDialog/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/DatePicker/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/DateRangePicker/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/DateTimePicker/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/DescriptionList/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/DialogSection/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Drawer/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/DropdownMenu/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/EmptyState/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/FieldWrapper/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/FloatingPanel/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/InlineEdit/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Input/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Label/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Modal/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/MultiSelect/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/MultiTypeCombobox/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/OverlayWidget/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/OwnerSelectBadge/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Pagination/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Popover/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/ProgressBar/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/PullToRefresh/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/RHFForm/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/RadioGroup/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/RangeSlider/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/SavedViews/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/SearchInput/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Section/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/SegmentedControl/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Select/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/SensitiveValueDisplay/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/ServerPagination/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Sheet/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Skeleton/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Skeleton/variants/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Slider/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/StatusBanner/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Stepper/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Switch/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/TableSortHeader/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Tabs/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Toast/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/ToastContainer/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/TokenPicker/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Toolbar/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Tooltip/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/ValidationSummary/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/ViewToggle/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/organisms/Carousel/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/organisms/ColumnVisibilityMenu/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/organisms/CommandPalette/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/organisms/DataTable/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/organisms/FileUpload/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/organisms/Form/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/organisms/RichTextEditor/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/organisms/Sidebar/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/BulkActionsToolbar/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/FilterBuilder/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/IntegrationCard/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/ActionBar/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/ActivityFeed/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/BulkEditDialog/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/CardFilter/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/CollectionView/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/EnhancedSearchBar/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/ExportDialog/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/FacetFilter/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/FilterBar/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/FilterPanel/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/KanbanBoard/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/LogicBuilder/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/ModalFilter/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/NotificationCenter/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/RangeSliderFilter/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/Timeline/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/ToggleFilter/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/UserMenu/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/flows/OnboardingTooltip/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/flows/OnboardingTooltips/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/flows/Wizard/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/templates/DashboardLayout/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/templates/DetailPage/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/templates/FormPage/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/templates/ListPage/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/templates/MasterDetail/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/templates/ResourceLayout/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/templates/SettingsPage/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/templates/WizardPage/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/utilities/VirtualizedTableBody/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/utilities/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/hooks/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/tokens/index.ts",
        "relation": "entrypoint_for"
      }
    ],
    "rejected_candidates": [],
    "confidence": "high"
  }
}
```

## Aethyme Pack

```json
{
  "anchors": {
    "task": "Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.",
    "anchors": [
      {
        "kind": "folder",
        "id": ".claude/commands",
        "file": null,
        "reason": "area match"
      }
    ]
  },
  "scope": {
    "task": "Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.",
    "navigation_order": [
      ".claude/commands"
    ],
    "in_scope_files": [],
    "in_scope_symbols": [],
    "in_scope_areas": [
      ".claude/commands"
    ],
    "out_of_scope": [
      ".chau7",
      ".chunk-history",
      ".claude",
      ".gcloud_access_token",
      ".gcloud_tmp",
      ".githooks",
      ".github",
      ".github/PULL_REQUEST_TEMPLATE",
      ".github/workflows",
      ".github/workflows/migrations-guard.yml",
      ".husky",
      ".hypothesis",
      ".lighthouseci",
      ".playwright-mcp",
      ".pnpm-store",
      ".pnpm-store/v10/index/17/3659f9b86de57d0529eeccc33dc3015026947d415796e549a93f9473012b3d-oauth4webapi@3.8.2.json",
      ".pnpm-store/v10/index/18/7b8344ed764b2a6ed9c57bd1dd5d900d845265c7827b6bcdba6f381f90cbee-comma-separated-tokens@1.0.8.json",
      ".pnpm-store/v10/index/29/afbd4ebbadbfb1bc33a593e927a2456cfbf762b9a84a881841b35ca84013ac-class-variance-authority@0.7.1.json",
      ".pnpm-store/v10/index/45/d2547e5704ddc5332a232a420b02bb4e853eef5474824ed1b7986cf8473789-js-tokens@4.0.0.json",
      ".pnpm-store/v10/index/55/dffd1150e2bba3cf26df72021eaba193fa125d711eb76f2151a3c81b074744-@csstools+css-tokenizer@3.0.4.json",
      ".pnpm-store/v10/index/59/dee61cf43ff33cba423edfe13e3abe0ddaa28afc7ec9099ba8366728f4eb8a-@auth+core@0.41.0.json",
      ".pnpm-store/v10/index/9b/16bd13d21314eb746da9f78fa2f93298f07a01b3ea505098cd4826459e0591-js-tokens@9.0.1.json",
      ".pnpm-store/v10/index/a3/69ee27ce43e04491c9b877cdb0390e5d4e7b5edf4592fefd0d7b6f5a90752f-@auth0+auth0-react@2.5.0.json",
      ".pnpm-store/v10/index/ab/f25255dd4ba6dce17f96e4626e286f88963e3c742a245edec44504dad5a9b2-space-separated-tokens@1.1.5.json",
      ".pnpm-store/v10/index/e1/7bf1d84e0dd808abaf5469f8a39e8dd0dba63e4b9df2ed359fd368e768ed56-@auth0+auth0-spa-js@2.5.0.json",
      ".pnpm-store/v10/index/f9/ce7582ab8cdc5ea73159a802eb1127b448a18d0ae13b3d1c20b0cb2fc14687-next-auth@5.0.0-beta.30.json",
      ".pnpm-store/v10/index/ff/b05db84885788349ee695cf22466aa9d2c0f0d9ada50056a18a0fd11a9a67e-eslint-plugin-no-secrets@2.2.1.json",
      ".secrets.baseline",
      ".storybook",
      ".wrangler",
      "Agents",
      "Agents/Skills Manager",
      "Agents/skills",
      "Agents/skills/auth/SKILL.md",
      "Agents/skills/auth/references/api-endpoints.md",
      "Agents/skills/auth/references/api-keys.md",
      "Agents/skills/auth/references/authentication.md",
      "Agents/skills/auth/references/common-patterns.md",
      "Agents/skills/auth/references/database-tables.md",
      "Agents/skills/auth/references/decisions.md",
      "Agents/skills/auth/references/learn-log.md",
      "Agents/skills/auth/references/rbac.md",
      "Agents/skills/auth/references/rbac.md",
      "Agents/skills/auth/references/security.md",
      "Agents/skills/auth/references/troubleshooting.md",
      "Agents/skills/ci-deploy/SKILL.md",
      "Agents/skills/ci-deploy/references/advanced-pipelines.md",
      "Agents/skills/ci-deploy/references/decisions.md",
      "Agents/skills/ci-deploy/references/docker.md",
      "Agents/skills/ci-deploy/references/gcp.md",
      "Agents/skills/ci-deploy/references/kubernetes.md",
      "Agents/skills/ci-deploy/references/learn-log.md",
      "Agents/skills/ci-deploy/references/pipelines.md",
      "Agents/skills/ci-deploy/references/secrets.md",
      "Agents/skills/ci-deploy/references/secrets.md",
      "Agents/skills/database/references/migrations.md",
      "Agents/skills/integrations/references/oauth-flows.md",
      "Agents/tasks",
      "Agents/tasks/2025-01-13-integrations-onboarding-oauth.md",
      "Agents/tasks/celery-cloudbuild-deploy.md",
      "Agents/tasks/celery-redis-secret-wiring.md",
      "Agents/tasks/dedicated-repo-migration.md",
      "Agents/tasks/fix-bootstrap-permission-case.md",
      "Agents/tasks/fix-environment-discovery-migration.md",
      "Agents/tasks/fix-mordor-roles-permissions-404.md",
      "Agents/tasks/fix-preauth-error-production.md",
      "Agents/tasks/google-oauth-onboarding.md",
      "Agents/tasks/merge-environment-0036-migrations.md",
      "Agents/tasks/otel-step1-deployment.md",
      "Agents/tasks/rbac-implementation-plan-intake.md",
      "Agents/tasks/rbac-pr5-pr8.md",
      "Agents/tasks/rbac-role-management-cleanup.md",
      "Agents/tasks/rbac-role-management-permissions.md",
      "Agents/tasks/role-management-permissions-check.md",
      "TODO",
      "alerts",
      "apps",
      "apps/customer",
      "apps/customer/src/entry-authenticated.tsx",
      "apps/mordor",
      "apps/mordor/src/entry-authenticated.tsx",
      "apps/organizations",
      "apps/organizations/src/entry-authenticated.tsx",
      "backend",
      "backend/MIGRATION_SCRIPT.py",
      "backend/accounts",
      "backend/accounts/admin_rbac_api_views.py",
      "backend/accounts/admin_rbac_views.py",
      "backend/accounts/auth0_management.py",
      "backend/accounts/auth_analytics_models.py",
      "backend/accounts/auth_analytics_serializers.py",
      "backend/accounts/auth_analytics_views.py",
      "backend/accounts/management/commands/rbac_dump_casl_catalog.py",
      "backend/accounts/management/commands/rbac_lifecycle_tick.py",
      "backend/accounts/management/commands/rbac_roles_summary.py",
      "backend/accounts/management/commands/rbac_seed_permissions.py",
      "backend/accounts/middleware_auth_enforcement.py",
      "backend/accounts/middleware_rbac_identity.py",
      "backend/accounts/migrations/0001_initial.py",
      "backend/accounts/migrations/0002_organization.py",
      "backend/accounts/migrations/0003_userprofile_org_default.py",
      "backend/accounts/migrations/0004_rls_userprofile.py",
      "backend/accounts/migrations/0005_tenant_membership.py",
      "backend/accounts/migrations/0006_userprofile_tenant_nullable.py",
      "backend/accounts/migrations/0007_seed_default_tenants_assign.py",
      "backend/accounts/migrations/0008_userprofile_tenant_nonnull.py",
      "backend/accounts/migrations/0009_rls_userprofile_tenant_update.py",
      "backend/accounts/migrations/0010_alter_userprofile_organization_and_more.py",
      "backend/accounts/migrations/0011_profile_identity_fields.py",
      "backend/accounts/migrations/0012_profile_phone_split.py",
      "backend/accounts/migrations/0013_team_and_identity_extras.py",
      "backend/accounts/migrations/0014_team_id_default.py",
      "backend/accounts/migrations/0015_userprofile_notification_prefs.py",
      "backend/accounts/migrations/0016_userprofile_tz_locale_notif_state.py",
      "backend/accounts/migrations/0017_tenant_notification_policy.py",
      "backend/accounts/migrations/0018_tenant_lifecycle_and_admin_models.py",
      "backend/accounts/migrations/0019_plan_entitlements.py",
      "backend/accounts/migrations/0020_alter_plandefinition_id_and_more.py",
      "backend/accounts/migrations/0021_internal_scopes_and_profile_flag.py",
      "backend/accounts/migrations/0022_custom_attributes.py",
      "backend/accounts/migrations/0023_team_user_custom.py",
      "backend/accounts/migrations/0024_rbac_registry.py",
      "backend/accounts/migrations/0024_rbac_registry.py",
      "backend/accounts/migrations/0025_role_archive.py",
      "backend/accounts/migrations/0025_search_trgm_indexes.py",
      "backend/accounts/migrations/0026_alter_customattributedefinition_id.py",
      "backend/accounts/migrations/0027_merge_20250922_0837.py",
      "backend/accounts/migrations/0028_permission_meta.py",
      "backend/accounts/migrations/0028_permission_meta.py",
      "backend/accounts/migrations/0028_role_risk_fields.py",
      "backend/accounts/migrations/0029_permission_metadata.py",
      "backend/accounts/migrations/0029_permission_metadata.py",
      "backend/accounts/migrations/0030_userprofile_ui_prefs.py",
      "backend/accounts/migrations/0031_enable_tenant_rls.py",
      "backend/accounts/migrations/0032_organization_hierarchy.py",
      "backend/accounts/migrations/0033_remove_organization_org_parent_idx_and_more.py",
      "backend/accounts/migrations/0034_check_constraints.py",
      "backend/accounts/migrations/0035_organization_profile_fields.py",
      "backend/accounts/migrations/0036_grc_organization_fields.py",
      "backend/accounts/migrations/0037_remove_sso_mfa_fields.py",
      "backend/accounts/migrations/0038_alter_organization_tax_id.py",
      "backend/accounts/migrations/0039_tenant_api_calls_month_tenant_api_calls_today_and_more.py",
      "backend/accounts/migrations/0040_tenant_admin_notification_message_and_more.py",
      "backend/accounts/migrations/0041_rolev2_organization_parent_userprofile_primary_team_and_more.py",
      "backend/accounts/migrations/0042_tenanthealthalertrule_tenanthealthmetric_and_more.py",
      "backend/accounts/migrations/0043_broadcasttemplate_scheduledbroadcast_and_more.py",
      "backend/accounts/migrations/0044_alter_scopedpermission_resource_roletemplate.py",
      "backend/accounts/migrations/0044_alter_scopedpermission_resource_roletemplate.py",
      "backend/accounts/migrations/0045_rolev2_tags.py",
      "backend/accounts/migrations/0046_remove_business_unit_and_update_team_types.py",
      "backend/accounts/migrations/0047_sync_rbac_permissions.py",
      "backend/accounts/migrations/0047_sync_rbac_permissions.py",
      "backend/accounts/migrations/0048_remove_userprofile_role.py",
      "backend/accounts/migrations/0049_alter_scopedpermission_resource.py",
      "backend/accounts/migrations/0049_alter_scopedpermission_resource.py",
      "backend/accounts/migrations/0999_rename_rolev2_to_role.py",
      "backend/accounts/migrations/1000_alter_role_options_alter_role_tenant.py",
      "backend/accounts/migrations/1001_drop_legacy_rbac_tables.py",
      "backend/accounts/migrations/1001_drop_legacy_rbac_tables.py",
      "backend/accounts/migrations/1002_recreate_role_permissions_through_table.py",
      "backend/accounts/migrations/1002_recreate_role_permissions_through_table.py",
      "backend/accounts/migrations/1003_alter_scopedpermission_resource.py",
      "backend/accounts/migrations/1003_alter_scopedpermission_resource.py",
      "backend/accounts/migrations/1004_alter_scopedpermission_action.py",
      "backend/accounts/migrations/1004_alter_scopedpermission_action.py",
      "backend/accounts/migrations/1005_add_device_and_session_models.py",
      "backend/accounts/migrations/1006_alter_scopedpermission_resource.py",
      "backend/accounts/migrations/1006_alter_scopedpermission_resource.py",
      "backend/accounts/migrations/1007_add_dashboard_resource.py",
      "backend/accounts/migrations/1008_entitlements_catalog.py",
      "backend/accounts/migrations/1009_seed_owner_internal.py",
      "backend/accounts/migrations/1010_subscription_split.py",
      "backend/accounts/migrations/1011_alter_catalogsubscription_id_alter_creditgrant_id_and_more.py",
      "backend/accounts/migrations/1012_merge_20251105_2056.py",
      "backend/accounts/migrations/1013_delete_rolev2_remove_role_archived_and_more.py",
      "backend/accounts/migrations/1014_notification_columns_and_locale_fields.py",
      "backend/accounts/migrations/1015_merge_20251122_2008.py",
      "backend/accounts/migrations/1016_add_account_models.py",
      "backend/accounts/migrations/1017_assign_demo_admin.py",
      "backend/accounts/migrations/1018_remove_demo_fullaccess_prod.py",
      "backend/accounts/migrations/1019_add_integrations_permissions.py",
      "backend/accounts/migrations/1019_add_integrations_permissions.py",
      "backend/accounts/migrations/1020_add_user_search_trgm_indexes.py",
      "backend/accounts/migrations/1021_role_risk_level_role_risk_meta_and_more.py",
      "backend/accounts/migrations/1022_userprofile_rls_by_user_id.py",
      "backend/accounts/migrations/1023_standardize_rls_gucs.py",
      "backend/accounts/migrations/1024_account_assetentity_fk.py",
      "backend/accounts/migrations/1025_account_roles_permissions_auth.py",
      "backend/accounts/migrations/1025_account_roles_permissions_auth.py",
      "backend/accounts/migrations/1025_account_roles_permissions_auth.py",
      "backend/accounts/migrations/1026_remove_account_accounts_ac_tenant__auth_idx_and_more.py",
      "backend/accounts/migrations/1026_remove_account_accounts_ac_tenant__auth_idx_and_more.py",
      "backend/accounts/migrations/1027_add_risk_rules_permissions.py",
      "backend/accounts/migrations/1027_add_risk_rules_permissions.py",
      "backend/accounts/migrations/1028_add_account_risk_fields.py",
      "backend/accounts/migrations/1029_add_finding_template_model.py",
      "backend/accounts/migrations/1030_role_is_template_role_source_template_and_more.py",
      "backend/accounts/migrations/1031_role_templates_global.py",
      "backend/accounts/migrations/1032_remove_role_accounts_role_template_requires_null_tenant_and_more.py",
      "backend/accounts/migrations/1033_drop_scopedpermission_and_legacy_role_fields.py",
      "backend/accounts/migrations/1033_drop_scopedpermission_and_legacy_role_fields.py",
      "backend/accounts/migrations/1034_alter_userroleassignment_scope_type_and_more.py",
      "backend/accounts/migrations/1035_roleriskpolicy.py",
      "backend/accounts/migrations/1036_account_oauth_scopes.py",
      "backend/accounts/migrations/1036_account_oauth_scopes.py",
      "backend/accounts/migrations/1037_add_external_avatar_url.py",
      "backend/accounts/migrations/1038_grant_demo_admin_v3.py",
      "backend/accounts/migrations/1039_rbac_homogenization.py",
      "backend/accounts/migrations/1039_rbac_homogenization.py",
      "backend/accounts/migrations/1040_remove_permissionauditlog_actor_and_more.py",
      "backend/accounts/migrations/1040_remove_permissionauditlog_actor_and_more.py",
      "backend/accounts/migrations/1041_alter_role_permissions_v3.py",
      "backend/accounts/migrations/1041_alter_role_permissions_v3.py",
      "backend/accounts/migrations/1042_migrate_permissions_to_canonical.py",
      "backend/accounts/migrations/1042_migrate_permissions_to_canonical.py",
      "backend/accounts/migrations/1043_access_grants_and_scope_types.py",
      "backend/accounts/migrations/1044_tenant_slug_global_unique.py",
      "backend/accounts/migrations/1045_rename_accounts_acc_grantor_status_idx_accounts_ac_grantor_970445_idx_and_more.py",
      "backend/accounts/migrations/1046_tenant_onboarding_apps_score_and_more.py",
      "backend/accounts/migrations/1047_tenant_dns_discovery_seed_fields.py",
      "backend/accounts/migrations/1048_authanalyticssummary_authevent.py",
      "backend/accounts/migrations/1048_authanalyticssummary_authevent.py",
      "backend/accounts/migrations/1048_seed_free_plan.py",
      "backend/accounts/migrations/1049_add_domain_role_exposure.py",
      "backend/accounts/migrations/1050_change_domain_role_to_roles_array.py",
      "backend/accounts/migrations/1051_tenant_profiles.py",
      "backend/accounts/migrations/1052_seed_tenant_profiles.py",
      "backend/accounts/migrations/1053_tenant_profile_templates.py",
      "backend/accounts/migrations/1054_seed_tenant_profile_templates.py",
      "backend/accounts/migrations/1055_role_templates_scope_and_profiles.py",
      "backend/accounts/migrations/1056_alter_role_organization_and_more.py",
      "backend/accounts/migrations/1057_tenantdomain_asset_entity.py",
      "backend/accounts/migrations/1058_role_template_visibility_and_auto_create.py",
      "backend/accounts/migrations/1059_fix_account_asset_fk_constraint.py",
      "backend/accounts/migrations/1060_enforce_userprofile_rls.py",
      "backend/accounts/migrations/1061_external_groups.py",
      "backend/accounts/migrations/1062_rename_accounts_ex_tenant__3a632a_idx_accounts_ex_tenant__0c1f4d_idx_and_more.py",
      "backend/accounts/migrations/1063_role_is_platform_staff.py",
      "backend/accounts/migrations/1064_platform_roles.py",
      "backend/accounts/migrations/1065_usersession_realm_enforcement.py",
      "backend/accounts/migrations/1066_remove_platformroleassignment_platform_role_assignment_user_role_uniq_and_more.py",
      "backend/accounts/migrations/1067_consolidate_data_models.py",
      "backend/accounts/migrations/1068_alter_organization_options_alter_team_options_and_more.py",
      "backend/accounts/migrations/1069_documentslot_and_status.py",
      "backend/accounts/migrations/1070_platform_role_assignment_starts_at.py",
      "backend/accounts/migrations/1071_merge_20260202_1350.py",
      "backend/accounts/migrations/1072_seed_default_platform_roles.py",
      "backend/accounts/migrations/1073_feature_key_allow_dots.py",
      "backend/accounts/migrations/1074_aeptus_support_access.py",
      "backend/accounts/migrations/1075_alter_usertenantmembership_role.py",
      "backend/accounts/migrations/1076_userprofile_rls_insert_policy.py",
      "backend/accounts/migrations/1077_migrate_platform_roles_to_canonical_rbac.py",
      "backend/accounts/migrations/1077_migrate_platform_roles_to_canonical_rbac.py",
      "backend/accounts/migrations/1078_userprofile_rls_include_memberships.py",
      "backend/accounts/migrations/1079_userprofile_archived_at.py",
      "backend/accounts/migrations/1080_profile_integrity_jobs.py",
      "backend/accounts/migrations/1081_impersonation_ticket_and_request_id.py",
      "backend/accounts/migrations/1082_alter_tenant_options.py",
      "backend/accounts/migrations/1083_alter_scheduledbroadcast_status.py",
      "backend/accounts/migrations/1084_tenant_profile_fk_and_framework_template.py",
      "backend/accounts/migrations/1085_seed_baseline_framework_templates.py",
      "backend/accounts/migrations/1086_merge_20260305_1932.py",
      "backend/accounts/migrations/FOLDER.migrations.md",
      "backend/accounts/migrations/__init__.py",
      "backend/accounts/permissions_base.py",
      "backend/accounts/rbac.py",
      "backend/accounts/rbac_audit_models.py",
      "backend/accounts/rbac_canonical.py",
      "backend/accounts/rbac_helpers.py",
      "backend/accounts/rbac_models.py",
      "backend/accounts/rbac_permissions.py",
      "backend/accounts/rbac_scope.py",
      "backend/accounts/rbac_signals.py",
      "backend/accounts/tests/test_rbac_access_engine.py",
      "backend/accounts/tests/test_rbac_lifecycle_tick.py",
      "backend/accounts/tests/test_rbac_on_behalf_audit.py",
      "backend/accounts/tests/test_rbac_team_auto_assign.py",
      "backend/adn",
      "backend/adn/migrations/0001_initial.py",
      "backend/adn/migrations/0002_enable_rls.py",
      "backend/adn/migrations/0003_fix_category_slug_uniqueness.py",
      "backend/adn/migrations/0004_pipelinerun_enrichmentqueue_directorysignal_and_more.py",
      "backend/adn/migrations/0005_localproviderentry_localserviceentry_and_more.py",
      "backend/adn/migrations/0006_remove_localproviderentry_unique_local_provider_domain_per_tenant_and_more.py",
      "backend/adn/migrations/0007_add_schema_version.py",
      "backend/adn/migrations/0008_directorycategory_expected_at_onboarding.py",
      "backend/adn/migrations/0009_add_app_metadata_facts.py",
      "backend/adn/migrations/0010_expand_fact_types.py",
      "backend/adn/migrations/0011_add_category_owner_fields.py",
      "backend/adn/migrations/0012_pipelinerun_add_adn_onboarding_enrich_stage.py",
      "backend/adn/migrations/0013_category_owner_delegation.py",
      "backend/adn/migrations/0014_pipelinestageconfig.py",
      "backend/adn/migrations/0015_remove_directoryfact_fact_single_target_entity_and_more.py",
      "backend/adn/migrations/0016_sitemap_supply_chain_choice_expansions.py",
      "backend/adn/migrations/0017_rename_enrichmentqueue_pipelinequeue.py",
      "backend/adn/migrations/0018_rename_adn_pipelin_target__70e8a1_idx_adn_pipelin_target__d13f85_idx_and_more.py",
      "backend/adn/migrations/0019_pipelinebatch.py",
      "backend/adn/migrations/0020_rename_adn_pipelin_status_batch_idx_adn_pipelin_status_90c11e_idx_and_more.py",
      "backend/adn/migrations/__init__.py",
      "backend/adn/permissions.py",
      "backend/adn/tests/test_permissions.py",
      "backend/aep_backend",
      "backend/ai_providers",
      "backend/ai_providers/migrations/0001_initial.py",
      "backend/ai_providers/migrations/0002_seed_providers.py",
      "backend/ai_providers/migrations/__init__.py",
      "backend/analytics",
      "backend/analytics/migrations/0001_initial.py",
      "backend/analytics/migrations/__init__.py",
      "backend/api_keys",
      "backend/api_keys/migrations/0001_initial.py",
      "backend/api_keys/migrations/0002_rename_api_keys_tenant__a3f8b1_idx_api_keys_tenant__aa40c3_idx_and_more.py",
      "backend/api_keys/migrations/0003_unique_constraints.py",
      "backend/api_keys/migrations/0004_apikey_user.py",
      "backend/api_keys/migrations/__init__.py",
      "backend/api_usage",
      "backend/api_usage/migrations/0001_initial.py",
      "backend/api_usage/migrations/0002_enable_rls.py",
      "backend/api_usage/migrations/0003_rename_api_deprec_tenant_status_idx_api_depreca_tenant__60e9d0_idx_and_more.py",
      "backend/api_usage/migrations/0004_brin_indexes.py",
      "backend/api_usage/migrations/0005_merge_20251001_1316.py",
      "backend/api_usage/migrations/0006_standardize_rls_gucs.py",
      "backend/api_usage/migrations/__init__.py",
      "backend/audit",
      "backend/audit/migrations/0001_initial.py",
      "backend/audit/migrations/0002_rls_and_brin.py",
      "backend/audit/migrations/0003_dedup_unique.py",
      "backend/audit/migrations/0003_partition_shadow_table.py",
      "backend/audit/migrations/0004_alter_auditeventv2_options_and_more.py",
      "backend/audit/migrations/0004_audit_export_job.py",
      "backend/audit/migrations/0005_audit_ingest_keys.py",
      "backend/audit/migrations/0005_audit_phase3_enhancements.py",
      "backend/audit/migrations/0006_auditpolicy_legal_hold.py",
      "backend/audit/migrations/0007_auditpolicy_retention_status.py",
      "backend/audit/migrations/0008_auditeventv2_perf_indexes.py",
      "backend/audit/migrations/0009_merge_0004_0008.py",
      "backend/audit/migrations/0010_drop_audit_event_legacy.py",
      "backend/audit/migrations/0011_alter_auditexportjob_id.py",
      "backend/audit/migrations/0012_auditexportjob_expires_at_auditexportjob_format_and_more.py",
      "backend/audit/migrations/0013_standardize_rls_gucs.py",
      "backend/audit/migrations/0014_actor_id_string.py",
      "backend/audit/migrations/FOLDER.migrations.md",
      "backend/audit/migrations/__init__.py",
      "backend/automations",
      "backend/automations/migrations/0001_initial.py",
      "backend/automations/migrations/0002_definition.py",
      "backend/automations/migrations/0003_data_model_rest.py",
      "backend/automations/migrations/0004_run_logs.py",
      "backend/automations/migrations/0005_enable_rls.py",
      "backend/automations/migrations/0006_check_constraints.py",
      "backend/automations/migrations/0007_performance_indexes.py",
      "backend/automations/migrations/0008_brin_indexes.py",
      "backend/automations/migrations/0009_partial_indexes.py",
      "backend/automations/migrations/0010_remove_automationdefinition_auto_def_tenant_status_idx_and_more.py",
      "backend/automations/migrations/0011_merge_20251106_2056.py",
      "backend/automations/migrations/0012_remove_automationdefinition_created_by_and_more.py",
      "backend/automations/migrations/0013_standardize_rls_gucs.py",
      "backend/automations/migrations/0014_eventdeadletter_payload_json.py",
      "backend/automations/migrations/__init__.py",
      "backend/collaboration",
      "backend/collaboration/migrations/0001_initial.py",
      "backend/collaboration/migrations/0002_review_models.py",
      "backend/collaboration/migrations/0003_rename_collaborati_locatio_idx_collaborati_locatio_8dcb41_idx_and_more.py",
      "backend/collaboration/migrations/__init__.py",
      "backend/common",
      "backend/community",
      "backend/community/migrations/0001_initial.py",
      "backend/community/migrations/0002_enable_rls_policies.py",
      "backend/community/migrations/0003_alter_implicitsignal_target_type.py",
      "backend/community/migrations/__init__.py",
      "backend/controls",
      "backend/controls/migrations/0001_initial.py",
      "backend/controls/migrations/0002_performance_indexes.py",
      "backend/controls/migrations/0003_custom_dashboards.py",
      "backend/controls/migrations/0004_remove_controldefinition_ctrl_tenant_status_domain_idx_and_more.py",
      "backend/controls/migrations/0005_add_control_assessment_items.py",
      "backend/controls/migrations/0006_add_scope_dsl_fields.py",
      "backend/controls/migrations/0007_rename_ctrl_item_tenant_occ_idx_controls_co_tenant__9e7728_idx_and_more.py",
      "backend/controls/migrations/0008_access_review_fields.py",
      "backend/controls/migrations/0009_item_validity_fields.py",
      "backend/controls/migrations/0009_rename_controls_co_tenant_c_kind_idx_controls_co_tenant__9e629c_idx_and_more.py",
      "backend/controls/migrations/0010_merge_20251007_1220.py",
      "backend/controls/migrations/0010_occurrence_signoff_fields.py",
      "backend/controls/migrations/0011_merge_20251007_1252.py",
      "backend/controls/migrations/0012_rename_controls_occ_signoff_due_idx_controls_co_signoff_7bb034_idx.py",
      "backend/controls/migrations/0013_controldefinition_business_unit_and_more.py",
      "backend/controls/migrations/0014_add_composite_indexes.py",
      "backend/controls/migrations/0014_add_performance_indexes.py",
      "backend/controls/migrations/0015_merge_20251104_0914.py",
      "backend/controls/migrations/0016_evidence_and_more.py",
      "backend/controls/migrations/0017_add_search_vector_control.py",
      "backend/controls/migrations/0018_controldefinition_idx_control_search.py",
      "backend/controls/migrations/0019_controldefinition_idx_control_search.py",
      "backend/controls/migrations/0020_enable_rls.py",
      "backend/controls/migrations/0021_evidence_artifact_scan_fields.py",
      "backend/controls/migrations/0022_framework_requirement_and_policy_mapping.py",
      "backend/controls/migrations/0023_populate_framework_requirements.py",
      "backend/controls/migrations/0024_rename_controls_fr_framewo_idx_cat_controls_fr_framewo_83042f_idx_and_more.py",
      "backend/controls/migrations/__init__.py",
      "backend/controls/permissions.py",
      "backend/controls/tests/test_permissions.py",
      "backend/controls/tests/test_rbac_boundary.py",
      "backend/core",
      "backend/core/management/commands/create_search_permissions.py",
      "backend/core/migrations/0001_initial.py",
      "backend/core/migrations/0003_inapp_security_evidence.py",
      "backend/core/migrations/0004_alerts_evidence_meta_url.py",
      "backend/core/migrations/0005_auditevent_healthcheck_and_more.py",
      "backend/core/migrations/0006_outbound_email_job.py",
      "backend/core/migrations/0007_emailjob_partial_idx.py",
      "backend/core/migrations/0008_outbound_email_bodyhash_unique.py",
      "backend/core/migrations/0009_remove_outboundemailjob_core_emailjob_triplet_uniq_and_more.py",
      "backend/core/migrations/0010_pg_stat_statements_extension.py",
      "backend/core/migrations/0011_delete_auditevent.py",
      "backend/core/migrations/0012_drop_core_auditevent.py",
      "backend/core/migrations/0013_rlsauditevent.py",
      "backend/core/migrations/0014_designsystempage_designsystemcomponent_and_more.py",
      "backend/core/migrations/0015_add_planned_components.py",
      "backend/core/migrations/0016_add_resource_permission_models.py",
      "backend/core/migrations/0016_add_resource_permission_models.py",
      "backend/core/migrations/0017_rlsauditevent.py",
      "backend/core/migrations/0018_change_default_visibility_to_tenant.py",
      "backend/core/migrations/0019_tenantattribute_moduleattributeconfig_and_more.py",
      "backend/core/migrations/0020_queryperformancelog_queryperformancestats.py",
      "backend/core/migrations/0021_rename_core_queryp_created_af2bd6_idx_core_queryp_created_ff0917_idx_and_more.py",
      "backend/core/migrations/0022_merge_20251106_2056.py",
      "backend/core/migrations/0023_search_analytics_models.py",
      "backend/core/migrations/0024_alter_searchanalytics_created_by_and_more.py",
      "backend/core/migrations/0025_export_job.py",
      "backend/core/migrations/0026_enable_rls_core_export_job.py",
      "backend/core/migrations/0027_alter_exportjob_format_alter_exportjob_status.py",
      "backend/core/migrations/FOLDER.migrations.md",
      "backend/core/migrations/__init__.py",
      "backend/core/permissions.py",
      "backend/core/permissions/__init__.py",
      "backend/core/permissions/decorators.py",
      "backend/core/permissions/helpers.py",
      "backend/core/permissions/policy.py",
      "backend/core/permissions/rbac.py",
      "backend/core/permissions/rls_queryset_manager.py",
      "backend/core/permissions/test_utils.py",
      "backend/core/permissions/tests/__init__.py",
      "backend/core/permissions/tests/test_decorators.py",
      "backend/core/permissions/tests/test_helpers.py",
      "backend/core/permissions/tests/test_policy.py",
      "backend/core/tests/test_search/test_search_permissions.py",
      "backend/deployment-guide.md",
      "backend/directory",
      "backend/directory/migrations/0001_initial.py",
      "backend/directory/migrations/0002_bitemporal_constraints.py",
      "backend/directory/migrations/0003_add_service_offering_technology.py",
      "backend/directory/migrations/0004_bitemporal_exclusion_constraints.py",
      "backend/directory/migrations/0005_check_constraints.py",
      "backend/directory/migrations/0006_alter_technologycomponent_unique_together_and_more.py",
      "backend/directory/migrations/0007_service_offering_technology_constraints_and_cleanup.py",
      "backend/directory/migrations/0008_legalentity_categories_serviceoffering_categories_and_more.py",
      "backend/directory/migrations/0009_legalentity_serviceoffering_expansion.py",
      "backend/directory/migrations/0010_legalentity_industry_sanctions_jurisdiction.py",
      "backend/directory/migrations/0011_allow_null_legal_entity_on_service_offering.py",
      "backend/directory/migrations/0012_remove_technologycomponent_categories_and_more.py",
      "backend/directory/migrations/0013_fix_techcat_null_distinct.py",
      "backend/directory/migrations/0014_remove_serviceoffering_tags.py",
      "backend/directory/migrations/0015_technologyproduct_categories.py",
      "backend/directory/migrations/0016_technologycategory_is_active.py",
      "backend/directory/migrations/__init__.py",
      "backend/docs",
      "backend/documents",
      "backend/documents/deployment-guide.md",
      "backend/documents/migrations/0001_initial.py",
      "backend/documents/migrations/0002_rename_doc_t_type_deleted_idx_documents_d_tenant__5ef7dd_idx_and_more.py",
      "backend/documents/migrations/0003_enable_rls.py",
      "backend/documents/migrations/0004_expand_doctype_and_relations.py",
      "backend/documents/migrations/0005_documentslot_and_status.py",
      "backend/documents/migrations/0006_documenttypeprofile.py",
      "backend/documents/migrations/__init__.py",
      "backend/environment",
      "backend/environment/migrations/0001_initial.py",
      "backend/environment/migrations/0002_add_owned_resource_mixin.py",
      "backend/environment/migrations/0002_initial.py",
      "backend/environment/migrations/0002_riskrule_riskrulefielddefinition_riskruleexecution.py",
      "backend/environment/migrations/0003_add_business_security_ownership.py",
      "backend/environment/migrations/0004_alter_asset_visibility_and_more.py",
      "backend/environment/migrations/0005_remove_asset_criticality_asset_service_asset_tier.py",
      "backend/environment/migrations/0006_add_composite_indexes.py",
      "backend/environment/migrations/0006_add_performance_indexes.py",
      "backend/environment/migrations/0007_merge_20251104_0914.py",
      "backend/environment/migrations/0008_remove_asset_env_asset_lifecycle_risk_idx_and_more.py",
      "backend/environment/migrations/0009_merge_20251106_2056.py",
      "backend/environment/migrations/0010_remove_asset_environment_owner_t_925258_idx_and_more.py",
      "backend/environment/migrations/0011_asset_idx_asset_type_tier_stat_and_more.py",
      "backend/environment/migrations/0012_add_search_vector_asset.py",
      "backend/environment/migrations/0013_asset_idx_asset_search.py",
      "backend/environment/migrations/0014_asset_managed_by_thirdpartyentity.py",
      "backend/environment/migrations/0015_alter_asset_unique_together_and_more.py",
      "backend/environment/migrations/0017_bitemporal_table_maintenance_tuning.py",
      "backend/environment/migrations/0018_asset_constraints_and_assettechnology_pair_unique.py",
      "backend/environment/migrations/0019_enable_rls.py",
      "backend/environment/migrations/0020_standardize_risk_fields.py",
      "backend/environment/migrations/0021_merge_20251216_1225.py",
      "backend/environment/migrations/0022_rename_env_riskrule_tenant_target_idx_environment_tenant__8ca752_idx_and_more.py",
      "backend/environment/migrations/0023_riskrulefielddefinition_category.py",
      "backend/environment/migrations/0024_add_asset_risk_breakdown.py",
      "backend/environment/migrations/0025_sprint14_asset_enhancements.py",
      "backend/environment/migrations/0026_rename_env_compmap_t_stat_idx_environment_tenant__24f617_idx_and_more.py",
      "backend/environment/migrations/0026_update_asset_search_vector_business_unit_option.py",
      "backend/environment/migrations/0027_merge_20251224_0905.py",
      "backend/environment/migrations/0028_asset_hosting_model_asset_local_service_and_more.py",
      "backend/environment/migrations/0029_asset_data_model_v12_1.py",
      "backend/environment/migrations/0030_remove_asset_idx_asset_category_and_more.py",
      "backend/environment/migrations/0031_risk_rule_library.py",
      "backend/environment/migrations/0032_remove_orgrulevisibility_unique_org_rule_visibility_and_more.py",
      "backend/environment/migrations/0033_add_asset_domain_registration_fields.py",
      "backend/environment/migrations/0034_alter_asset_asset_type.py",
      "backend/environment/migrations/0035_domain_analyzer_integration_sprint16.py",
      "backend/environment/migrations/0036_asset_discovery_tracking_fields.py",
      "backend/environment/migrations/0036_rename_idx_certhistory_asset_environment_tenant__3daa83_idx_and_more.py",
      "backend/environment/migrations/0037_merge_20260109_0700.py",
      "backend/environment/migrations/0038_threat_intelligence_traffic_ranking.py",
      "backend/environment/migrations/0039_remove_asset_idx_asset_threat_malicious_and_more.py",
      "backend/environment/migrations/0040_technology_fingerprinting.py",
      "backend/environment/migrations/0041_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
      "backend/environment/migrations/0042_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
      "backend/environment/migrations/0043_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
      "backend/environment/migrations/0044_asset_discovery_sources.py",
      "backend/environment/migrations/0045_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
      "backend/environment/migrations/0046_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
      "backend/environment/migrations/0047_update_threatintelligencecheck_constraint.py",
      "backend/environment/migrations/0048_backfill_asset_discovery_sources.py",
      "backend/environment/migrations/0049_asset_directory_category.py",
      "backend/environment/migrations/0050_remove_technologycategory_parent_and_more.py",
      "backend/environment/migrations/0051_remove_asset_business_function_id_and_more.py",
      "backend/environment/migrations/__init__.py",
      "backend/environment/permissions.py",
      "backend/environment/risk_rule_library_permissions.py",
      "backend/environment/risk_rule_permissions.py",
      "backend/events",
      "backend/events/migrations/0001_initial.py",
      "backend/events/migrations/0002_add_owned_resource_mixin.py",
      "backend/events/migrations/0003_add_business_security_ownership.py",
      "backend/events/migrations/0004_alter_event_visibility_and_more.py",
      "backend/events/migrations/0005_incident.py",
      "backend/events/migrations/0006_delete_incident.py",
      "backend/events/migrations/0007_incident_assetentity.py",
      "backend/events/migrations/0008_alter_incident_created_at_alter_incident_created_by_and_more.py",
      "backend/events/migrations/0009_alter_incident_business_owner_team_and_more.py",
      "backend/events/migrations/__init__.py",
      "backend/frameworks",
      "backend/frameworks/migrations/__init__.py",
      "backend/gcp/deploy.sh",
      "backend/gcp/setup-search-infrastructure.sh",
      "backend/guide-migrations.md",
      "backend/information",
      "backend/information/migrations/0001_initial.py",
      "backend/information/migrations/0001_initial_ims_models.py",
      "backend/information/migrations/0002_alter_document_content_type_and_more.py",
      "backend/information/migrations/0003_merge_20251106_2056.py",
      "backend/information/migrations/0004_assetentity_fks_and_contenttypes.py",
      "backend/information/migrations/0005_alter_privacyprofile_content_type.py",
      "backend/information/migrations/__init__.py",
      "backend/information/models/migration.py",
      "backend/information/serializers/migration.py",
      "backend/integrations",
      "backend/integrations/migrations/0001_initial.py",
      "backend/integrations/migrations/0002_alter_integrationconnection_provider.py",
      "backend/integrations/migrations/0003_slackinstall.py",
      "backend/integrations/migrations/0004_rename_integrations_slack_tenant_team_idx_integration_tenant__f6ace6_idx.py",
      "backend/integrations/migrations/0005_enhance_integrationconnection_for_adapter_pattern.py",
      "backend/integrations/migrations/0006_enhance_integration_connection.py",
      "backend/integrations/migrations/0007_integrationfieldmapping.py",
      "backend/integrations/migrations/0008_scaling_architecture.py",
      "backend/integrations/migrations/0009_alter_integrationprovider_options_and_more.py",
      "backend/integrations/migrations/0010_integrationaction_integrationdatapoint.py",
      "backend/integrations/migrations/0011_seed_google_workspace_actions_complete.py",
      "backend/integrations/migrations/0012_integrationaction_category.py",
      "backend/integrations/migrations/0013_seed_google_workspace_webhooks.py",
      "backend/integrations/migrations/0014_seed_slack_provider_and_actions.py",
      "backend/integrations/migrations/0015_seed_github_provider_and_actions.py",
      "backend/integrations/migrations/0016_add_is_automation_enabled.py",
      "backend/integrations/migrations/0017_integrationaction_integration_auto_en_idx_and_more.py",
      "backend/integrations/migrations/0018_integration_sync_history.py",
      "backend/integrations/migrations/0019_add_sync_history_data_snapshots.py",
      "backend/integrations/migrations/0020_add_sync_type_choices.py",
      "backend/integrations/migrations/0021_rename_nango_connection_id.py",
      "backend/integrations/migrations/0022_normalize_integration_provider_categories.py",
      "backend/integrations/migrations/0023_seed_microsoft_365_provider.py",
      "backend/integrations/migrations/0024_seed_microsoft_teams_provider.py",
      "backend/integrations/migrations/0025_normalize_connected_status_to_active.py",
      "backend/integrations/migrations/0026_seed_google_workspace_provider.py",
      "backend/integrations/migrations/__init__.py",
      "backend/integrations/tests/test_token_lifecycle_guardrails.py",
      "backend/integrations/tests/test_token_refresh.py",
      "backend/integrations/token-lifecycle-standard.md",
      "backend/k8s",
      "backend/knowledge",
      "backend/knowledge/migrations/0001_initial.py",
      "backend/knowledge/migrations/0002_remove_controlmapping_unique_policy_requirement_and_more.py",
      "backend/knowledge/migrations/__init__.py",
      "backend/localization",
      "backend/localization/migrations/0001_initial.py",
      "backend/localization/migrations/0002_add_owned_resource_mixin.py",
      "backend/localization/migrations/0003_add_business_security_ownership.py",
      "backend/localization/migrations/0004_alter_glossaryterm_visibility_and_more.py",
      "backend/localization/migrations/0005_add_analytics_models.py",
      "backend/localization/migrations/0006_alter_translationchangelog_created_by_and_more.py",
      "backend/localization/migrations/0007_translation_ai_config.py",
      "backend/localization/migrations/__init__.py",
      "backend/manual-deploy-with-verify.sh",
      "backend/mapping_intelligence",
      "backend/mapping_intelligence/migrations/0001_initial.py",
      "backend/mapping_intelligence/migrations/0002_add_missing_fields.py",
      "backend/mapping_intelligence/migrations/0002_fielddefinition_mapping_int_synonym_49b140_gin.py",
      "backend/mapping_intelligence/migrations/0003_add_aimachinesettings.py",
      "backend/mapping_intelligence/migrations/0003_fielddefinition_tenant_scope.py",
      "backend/mapping_intelligence/migrations/0003_rename_mapping_int_entity__idx_mapping_int_entity__9ab5a0_idx_and_more.py",
      "backend/mapping_intelligence/migrations/0004_merge_20251020_1449.py",
      "backend/mapping_intelligence/migrations/0005_add_versioning_and_constraints.py",
      "backend/mapping_intelligence/migrations/0007_add_performance_indexes.py",
      "backend/mapping_intelligence/migrations/0008_merge_20251106_2056.py",
      "backend/mapping_intelligence/migrations/0009_mappinghistory_updated_at_mappinghistory_updated_by_and_more.py",
      "backend/mapping_intelligence/migrations/0010_merge_20260105_1105.py",
      "backend/mapping_intelligence/migrations/0011_remove_fielddefinition_mapping_int_entity__030d87_idx_and_more.py",
      "backend/mapping_intelligence/migrations/__init__.py",
      "backend/mapping_intelligence/permissions.py",
      "backend/menu_overrides",
      "backend/menu_overrides/migrations/0001_initial.py",
      "backend/menu_overrides/migrations/0002_add_navigation_analytics.py",
      "backend/menu_overrides/migrations/__init__.py",
      "backend/middleware",
      "backend/middleware/rbac_enforcement.py",
      "backend/onboarding",
      "backend/onboarding/migrations/0001_initial.py",
      "backend/onboarding/migrations/0002_onboardingruntimestate.py",
      "backend/onboarding/migrations/__init__.py",
      "backend/operational",
      "backend/operational/migrations/0001_initial.py",
      "backend/operational/migrations/0002_event_sourcing_triggers.py",
      "backend/operational/migrations/0003_fix_event_sourcing_trigger.py",
      "backend/operational/migrations/0004_trigger_request_id.py",
      "backend/operational/migrations/0005_trigger_request_id_metadata.py",
      "backend/operational/migrations/0006_trigger_update_merge_guard.py",
      "backend/operational/migrations/0007_trigger_merge_guard_jsonb.py",
      "backend/operational/migrations/__init__.py",
      "backend/ops/scripts/deploy-celery-jobs.sh",
      "backend/ops/scripts/deploy-rbac-seed.sh",
      "backend/ops/scripts/deploy-rbac-seed.sh",
      "backend/ops/scripts/execute-rbac-seed.sh",
      "backend/ops/scripts/run-migrations.sh",
      "backend/ops/scripts/seed-rbac-permissions.sh",
      "backend/page_actions",
      "backend/page_actions/migrations/0001_initial.py",
      "backend/page_actions/migrations/0002_remove_customaction_unique_custom_action_per_org_page_and_more.py",
      "backend/page_actions/migrations/0003_standardize_rls_gucs.py",
      "backend/page_actions/migrations/__init__.py",
      "backend/page_actions/permissions.py",
      "backend/page_actions/services/permission_service.py",
      "backend/page_actions/tests/test_permission_service.py",
      "backend/posture",
      "backend/posture/finding_template_library_permissions.py",
      "backend/posture/migrations/0001_initial.py",
      "backend/posture/migrations/0002_add_owned_resource_mixin.py",
      "backend/posture/migrations/0003_add_business_security_ownership.py",
      "backend/posture/migrations/0004_alter_campaign_visibility_alter_finding_visibility_and_more.py",
      "backend/posture/migrations/0005_add_search_vector_finding.py",
      "backend/posture/migrations/0006_finding_idx_finding_search.py",
      "backend/posture/migrations/0007_alter_campaign_scope_assets_alter_finding_asset_and_more.py",
      "backend/posture/migrations/0008_add_finding_likelihood_and_targets.py",
      "backend/posture/migrations/0009_add_finding_template_model.py",
      "backend/posture/migrations/0010_finding_template_library.py",
      "backend/posture/migrations/0011_seed_finding_template_library.py",
      "backend/posture/migrations/0012_rename_posture_ftl_category_status_idx_posture_ftl_cat_status_idx_and_more.py",
      "backend/posture/migrations/__init__.py",
      "backend/project",
      "backend/reports",
      "backend/run_migrations.py",
      "backend/scripts",
      "backend/scripts/audit_rbac_migration.py",
      "backend/scripts/audit_rbac_migration.py",
      "backend/scripts/debug/test_automations_permission_debug.py",
      "backend/scripts/debug/test_rbac_migration.py",
      "backend/scripts/debug/test_rbac_migration.py",
      "backend/setup-auto-deploy.sh",
      "backend/tasks",
      "backend/tasks/migrations/0001_initial.py",
      "backend/tasks/migrations/0002_tasklink.py",
      "backend/tasks/migrations/0003_tasksavedview.py",
      "backend/tasks/migrations/0004_task_tags.py",
      "backend/tasks/migrations/0005_task_comments_watchers.py",
      "backend/tasks/migrations/0006_task_attachment.py",
      "backend/tasks/migrations/0007_checklist_item.py",
      "backend/tasks/migrations/0008_alter_checklistitem_created_at_and_more.py",
      "backend/tasks/migrations/0008_task_workflow_sla.py",
      "backend/tasks/migrations/0009_task_provenance.py",
      "backend/tasks/migrations/0010_merge_20251006_1220.py",
      "backend/tasks/migrations/0011_add_owned_resource_mixin.py",
      "backend/tasks/migrations/0016_task_completion_rule.py",
      "backend/tasks/migrations/0017_add_business_security_ownership.py",
      "backend/tasks/migrations/0018_alter_checklistitem_visibility_alter_task_visibility_and_more.py",
      "backend/tasks/migrations/0019_add_search_vector_task.py",
      "backend/tasks/migrations/0020_task_idx_task_search.py",
      "backend/tasks/migrations/0021_enable_rls.py",
      "backend/tasks/migrations/0022_add_task_decisions.py",
      "backend/tasks/migrations/0023_convert_task_decision_to_task_type.py",
      "backend/tasks/migrations/0024_enforce_single_pending_task_decision.py",
      "backend/tasks/migrations/0025_alter_task_status_alter_task_type.py",
      "backend/tasks/migrations/__init__.py",
      "backend/templates",
      "backend/test-results",
      "backend/tests",
      "backend/tests/admin/test_admin_notifications_policy_rbac.py",
      "backend/tests/admin/test_admin_permission_audit.py",
      "backend/tests/admin/test_admin_permission_audit_correlation.py",
      "backend/tests/admin/test_admin_roles_rbac_edit_allow.py",
      "backend/tests/admin/test_admin_roles_rbac_edit_deny.py",
      "backend/tests/admin/test_admin_users_rbac_allow.py",
      "backend/tests/audit/test_audit_events_rbac_deny_audit.py",
      "backend/tests/audit/test_audit_export_rbac_deny.py",
      "backend/tests/audit/test_audit_export_rbac_superuser.py",
      "backend/tests/audit/test_audit_rbac.py",
      "backend/tests/audit/test_audit_rbac_endpoints.py",
      "backend/tests/audit/test_audit_registry_billing_vendor_required.py",
      "backend/tests/audit/test_audit_registry_permissions_required.py",
      "backend/tests/critical/test_auth_oidc.py",
      "backend/tests/integration/test_auth0_idp_asset_bootstrap.py",
      "backend/tests/integration/test_collaboration_authorization.py",
      "backend/tests/integration/test_db_viewer_rbac_allow.py",
      "backend/tests/integration/test_schema_permissions.py",
      "backend/tests/integration/test_schema_ui_permissions.py",
      "backend/tests/integration/test_secret_hashing.py",
      "backend/tests/integration/test_teams_rbac.py",
      "backend/tests/integration/test_teams_rbac_allow.py",
      "backend/tests/integration/test_thirdparty_authorization.py",
      "backend/tests/integration/test_thirdparty_relationship_authorization.py",
      "backend/tests/security/test_auth_flows_comprehensive.py",
      "backend/tests/security/test_auth_login_ratelimit.py",
      "backend/tests/security/test_auth_logout_csrf.py",
      "backend/tests/security/test_auth_session.py",
      "backend/tests/security/test_impersonation_rbac.py",
      "backend/tests/security/test_rbac_admin_api.py",
      "backend/tests/security/test_rbac_casl_mapping.py",
      "backend/tests/security/test_rbac_forbidden_json.py",
      "backend/tests/security/test_rbac_forbidden_json_shape.py",
      "backend/tests/security/test_rbac_risk_matrix.py",
      "backend/tests/security/test_rbac_risk_recalc_command.py",
      "backend/tests/security/test_rbac_settings_guard.py",
      "backend/tests/security/test_thirdparty_unauth_endpoints.py",
      "backend/tests/suppliers/test_suppliers_reports_rbac.py",
      "backend/thirdparties",
      "backend/thirdparties/migrations/0001_initial.py",
      "backend/thirdparties/migrations/0002_enable_rls_policies.py",
      "backend/thirdparties/migrations/0003_bitemporal_constraints.py",
      "backend/thirdparties/migrations/0004_rename_tables_suppliers_to_thirdparties.py",
      "backend/thirdparties/migrations/0005_rename_thirdparties_a_tenant__c63003_idx_thirdpartie_tenant__0c77fd_idx_and_more.py",
      "backend/thirdparties/migrations/0006_rename_thirdparties_a_tenant__c63003_idx_thirdpartie_tenant__0c77fd_idx_and_more.py",
      "backend/thirdparties/migrations/0007_add_asset_service_offering.py",
      "backend/thirdparties/migrations/0008_rename_thirdparties_a_tenant__c63003_idx_thirdpartie_tenant__0c77fd_idx_and_more.py",
      "backend/thirdparties/migrations/0009_supplier_graph_view.py",
      "backend/thirdparties/migrations/0010_add_missing_fields.py",
      "backend/thirdparties/migrations/0011_enable_rls.py",
      "backend/thirdparties/migrations/0012_remove_thirdpartyrelationship_thirdparty_rel_unique_and_more.py",
      "backend/thirdparties/migrations/0012_suppliers_saved_view.py",
      "backend/thirdparties/migrations/0013_asset_service_offering_constraints.py",
      "backend/thirdparties/migrations/0014_check_constraints.py",
      "backend/thirdparties/migrations/0015_performance_indexes.py",
      "backend/thirdparties/migrations/0016_partial_indexes.py",
      "backend/thirdparties/migrations/0017_add_frontend_aligned_fields.py",
      "backend/thirdparties/migrations/0018_add_gdpr_data_privacy_models.py",
      "backend/thirdparties/migrations/0019_remove_dataprivacycontact_created_by_and_more.py",
      "backend/thirdparties/migrations/0020_add_privacy_enhancements.py",
      "backend/thirdparties/migrations/0021_remove_dataprivacycontact_created_by_and_more.py",
      "backend/thirdparties/migrations/0022_repair_privacy_columns.py",
      "backend/thirdparties/migrations/0023_remove_asset_asset_tenant_type_idx_and_more.py",
      "backend/thirdparties/migrations/0024_remove_asset_asset_tenant_type_idx_and_more.py",
      "backend/thirdparties/migrations/0025_alter_document_content_type_and_more.py",
      "backend/thirdparties/migrations/0026_supplierassessment_supplierchangerequest_and_more.py",
      "backend/thirdparties/migrations/0027_dataprivacycontact_dataprivacyprofile_and_more.py",
      "backend/thirdparties/migrations/0028_merge_20251023_1923.py",
      "backend/thirdparties/migrations/0029_tprm_policy_owner_team_doc_source.py",
      "backend/thirdparties/migrations/0030_rename_tp_tenant_owner_user_idx_thirdpartie_tenant__4ef8e4_idx_and_more.py",
      "backend/thirdparties/migrations/0031_add_business_security_ownership.py",
      "backend/thirdparties/migrations/0032_dataprivacycontact_dataprivacyprofile_and_more.py",
      "backend/thirdparties/migrations/0033_alter_thirdparty_visibility.py",
      "backend/thirdparties/migrations/0034_alter_thirdparty_relationship_types.py",
      "backend/thirdparties/migrations/0035_thirdparty_frameworks_alter_thirdparty_tags.py",
      "backend/thirdparties/migrations/0036_alter_thirdparty_tags.py",
      "backend/thirdparties/migrations/0037_add_composite_indexes.py",
      "backend/thirdparties/migrations/0037_add_performance_indexes.py",
      "backend/thirdparties/migrations/0038_merge_20251104_0914.py",
      "backend/thirdparties/migrations/0039_remove_directorylinkconfig_tp_link_sync_idx_and_more.py",
      "backend/thirdparties/migrations/0041_search_extensions_and_indexes.py",
      "backend/thirdparties/migrations/0042_add_search_vector_thirdparty.py",
      "backend/thirdparties/migrations/0043_thirdparty_idx_thirdparty_search.py",
      "backend/thirdparties/migrations/0044_thirdparty_entity_versioning.py",
      "backend/thirdparties/migrations/0045_dataprivacyprofile_third_party_entity.py",
      "backend/thirdparties/migrations/0046_rename_thirdparty_tenant_entity_idx_thirdpartie_tenant__205199_idx_and_more.py",
      "backend/thirdparties/migrations/0047_standardize_rls_gucs.py",
      "backend/thirdparties/migrations/0048_alter_directorylinkconfig_linked_legal_entity_and_more.py",
      "backend/thirdparties/migrations/0049_fix_thirdparty_no_overlap_valid_to_infinity.py",
      "backend/thirdparties/migrations/0050_bitemporal_table_maintenance_tuning.py",
      "backend/thirdparties/migrations/0051_standardize_risk_fields.py",
      "backend/thirdparties/migrations/0052_alter_thirdparty_risk_factors_and_more.py",
      "backend/thirdparties/migrations/0053_directorylinkconfig_linked_local_provider.py",
      "backend/thirdparties/migrations/0054_alter_thirdparty_lifecycle_status.py",
      "backend/thirdparties/migrations/0055_thirdparty_adn_parity_fields.py",
      "backend/thirdparties/migrations/0056_functionalrole_industrycodecrosswalk_and_more.py",
      "backend/thirdparties/migrations/0057_seed_functional_roles.py",
      "backend/thirdparties/migrations/0058_seed_industry_crosswalk.py",
      "backend/thirdparties/migrations/0059_thirdparty_adn_parity_fields.py",
      "backend/thirdparties/migrations/0060_supplier_directory_category.py",
      "backend/thirdparties/migrations/0061_thirdparty_control_frameworks.py",
      "backend/thirdparties/migrations/0062_migrate_frameworks_m2m.py",
      "backend/thirdparties/migrations/0063_alter_thirdparty_frameworks.py",
      "backend/thirdparties/migrations/FOLDER.migrations.md",
      "backend/thirdparties/migrations/__init__.py",
      "backend/webhooks",
      "backend/webhooks/migrations/0001_initial.py",
      "backend/webhooks/migrations/0002_rename_webhooks_de_subscri_f5d8c1_idx_webhooks_de_subscri_f97236_idx_and_more.py",
      "backend/webhooks/migrations/0003_unique_constraints.py",
      "backend/webhooks/migrations/0004_add_owned_resource_mixin.py",
      "backend/webhooks/migrations/0005_add_business_security_ownership.py",
      "backend/webhooks/migrations/0006_alter_webhookdelivery_visibility_and_more.py",
      "backend/webhooks/migrations/0007_hash_webhook_secrets.py",
      "backend/webhooks/migrations/0007_hash_webhook_secrets.py",
      "backend/webhooks/migrations/0008_alter_webhooksubscription_secret.py",
      "backend/webhooks/migrations/0008_alter_webhooksubscription_secret.py",
      "backend/webhooks/migrations/__init__.py",
      "catalog",
      "config",
      "config/bundle",
      "config/lighthouse",
      "config/observability",
      "config/quality",
      "contracts",
      "contracts/config",
      "contracts/migration-to-schemathesis.md",
      "devops",
      "devops/grafana/dashboards/rbac-dashboard.json",
      "devops/prometheus/rules/rbac-alerts.yml",
      "docker",
      "docs",
      "docs/access-auth.md",
      "docs/adr",
      "docs/agents",
      "docs/agents/context/admin.billing.md",
      "docs/api",
      "docs/api/rbac-api-reference.md",
      "docs/api/rbac-api.md",
      "docs/api/rbac-openapi.json",
      "docs/api/rbac-quick-reference.md",
      "docs/architecture",
      "docs/architecture/architecture-deployment.md",
      "docs/architecture/rbac-architecture.md",
      "docs/architecture/security-audit-rbac.md",
      "docs/badges",
      "docs/collaboration",
      "docs/contracts",
      "docs/db",
      "docs/design-system",
      "docs/design-system/automated-deployment-setup.md",
      "docs/design-system/design-tokens-tier-guide.md",
      "docs/development",
      "docs/docker",
      "docs/engineering",
      "docs/feature-flags/catalog-key-migration.md",
      "docs/feature-specs",
      "docs/feature-specs/admin/page-actions/09-deployment-guide.md",
      "docs/feature-specs/controls/deployment-checklist.md",
      "docs/feature-specs/information/my-environment-rbac-integration.md",
      "docs/feature-specs/rbac/admin-roles-review-and-cleanup.md",
      "docs/feature-specs/rbac/rbac-spec.md",
      "docs/feature-specs/search-deployment-guide.md",
      "docs/guides",
      "docs/guides/authentication-setup.md",
      "docs/guides/cost-optimized-deployment.md",
      "docs/guides/deployment-guide-permissions.md",
      "docs/guides/deployment-guide-permissions.md",
      "docs/guides/multi-tenant-deployment-critical.md",
      "docs/guides/post-deployment-setup.md",
      "docs/guides/post-deployment-verification.md",
      "docs/guides/rbac-admin-guide.md",
      "docs/observability",
      "docs/onboarding",
      "docs/openapi",
      "docs/otlp",
      "docs/performance",
      "docs/permissions.md",
      "docs/planning",
      "docs/plans",
      "docs/plans/infrastructure-options-comparison.md",
      "docs/prd",
      "docs/prd/rbac-simplified-design.md",
      "docs/rbac-cache-implementation.md",
      "docs/reference",
      "docs/reference/rbac.yaml",
      "docs/reference/reference-rbac-permission-sync.md",
      "docs/reports",
      "docs/runbooks",
      "docs/runbooks/deploy-admin.md",
      "docs/runbooks/deployment-checklist.md",
      "docs/runbooks/rbac-operations-runbook.md",
      "docs/runbooks/rbac-risk-policy.md",
      "docs/runbooks/runbook-deployment-best-practices.md",
      "docs/runbooks/runbook-production-deployment.md",
      "docs/secret-management-plan.md",
      "docs/security",
      "docs/security/rbac-risk-policy.md",
      "docs/testing",
      "e2e",
      "e2e/fixtures",
      "e2e/fixtures/auth.fixture.ts",
      "e2e/page-objects",
      "e2e/page-objects/auth/login.page.ts",
      "e2e/tests/auth/authentication.spec.ts",
      "functions",
      "gcp-run-proxy",
      "gcp-run-proxy/src",
      "gcp-run-proxy/test",
      "grafana-provisioning",
      "load_tests",
      "logs",
      "manual-deployment-steps.md",
      "migration-complete.md",
      "migration-status.md",
      "migrations-applied-success.md",
      "output",
      "packages",
      "packages/app-shared",
      "packages/app-shared/src/app/AuthenticatedApp.tsx",
      "packages/app-shared/src/auth/AbilityContext.shared.ts",
      "packages/app-shared/src/auth/AbilityProvider.ts",
      "packages/app-shared/src/auth/AbilityProviderRoot.tsx",
      "packages/app-shared/src/auth/AuthError.tsx",
      "packages/app-shared/src/auth/FOLDER.auth.md",
      "packages/app-shared/src/auth/NoTenantAccess.tsx",
      "packages/app-shared/src/auth/SessionExpiryWarningProvider.tsx",
      "packages/app-shared/src/auth/SessionGate.tsx",
      "packages/app-shared/src/auth/UseEnvironmentPermissions.tsx",
      "packages/app-shared/src/auth/UseEnvironmentPermissions.tsx",
      "packages/app-shared/src/auth/__tests__/FOLDER.__tests__.md",
      "packages/app-shared/src/auth/__tests__/permissionGrouping.test.ts",
      "packages/app-shared/src/auth/__tests__/permissionGrouping.test.ts",
      "packages/app-shared/src/auth/ability.ts",
      "packages/app-shared/src/auth/can.ts",
      "packages/app-shared/src/auth/logoutBroadcast.ts",
      "packages/app-shared/src/auth/logoutClient.ts",
      "packages/app-shared/src/auth/permissionGrouping.ts",
      "packages/app-shared/src/auth/permissionGrouping.ts",
      "packages/app-shared/src/auth/rbac-canonical.ts",
      "packages/app-shared/src/auth/rbac-canonical.ts",
      "packages/app-shared/src/auth/session.ts",
      "packages/app-shared/src/auth/sessionExpiryWarningContext.ts",
      "packages/app-shared/src/auth/useSessionHeartbeat.ts",
      "packages/app-shared/src/components/admin/AdminBillingView.tsx",
      "packages/app-shared/src/components/admin/OrgBillingOverviewView.tsx",
      "packages/app-shared/src/components/admin/TenantBillingTab.tsx",
      "packages/app-shared/src/components/admin/roles/BatchPermissionUpdates.tsx",
      "packages/app-shared/src/components/admin/roles/PermissionConflictDetector.tsx",
      "packages/app-shared/src/components/admin/roles/PermissionMatrix.tsx",
      "packages/app-shared/src/components/admin/roles/PermissionMatrixSkeleton.tsx",
      "packages/app-shared/src/components/admin/roles/PermissionsList.tsx",
      "packages/app-shared/src/components/admin/roles/__tests__/BatchPermissionUpdates.test.tsx",
      "packages/app-shared/src/components/auth/PermissionDenied.tsx",
      "packages/app-shared/src/components/auth/PermissionDenied.tsx",
      "packages/app-shared/src/constants/rbac-module-settings.md",
      "packages/app-shared/src/constants/rbac.ts",
      "packages/app-shared/src/features/admin/components/AdminBillingView.tsx",
      "packages/app-shared/src/features/admin/components/roles/PermissionConflictDetector.tsx",
      "packages/app-shared/src/features/admin/components/roles/PermissionMatrix.tsx",
      "packages/app-shared/src/features/admin/components/roles/PermissionMatrixSkeleton.tsx",
      "packages/app-shared/src/features/admin/components/roles/permissionConflictRules.ts",
      "packages/app-shared/src/features/admin/components/roles/permissionMatrix.shared.ts",
      "packages/app-shared/src/features/admin/hooks/useUsersAndPermissions.ts",
      "packages/app-shared/src/features/admin/pages/AdminBillingPage.tsx",
      "packages/app-shared/src/features/admin/pages/AdminMigrationDashboardPage.tsx",
      "packages/app-shared/src/features/admin/pages/AdminMigrationDashboardPageView.tsx",
      "packages/app-shared/src/features/auth/components/PermissionDenied.tsx",
      "packages/app-shared/src/features/auth/components/PermissionDenied.tsx",
      "packages/app-shared/src/features/auth/components/index.ts",
      "packages/app-shared/src/features/auth/index.ts",
      "packages/app-shared/src/features/auth/utils/ability.ts",
      "packages/app-shared/src/features/auth/utils/can.ts",
      "packages/app-shared/src/features/auth/utils/index.ts",
      "packages/app-shared/src/features/auth/utils/permissionGrouping.ts",
      "packages/app-shared/src/features/auth/utils/permissionGrouping.ts",
      "packages/app-shared/src/features/auth/utils/session.ts",
      "packages/app-shared/src/features/information/hooks/useMigration.ts",
      "packages/app-shared/src/features/information/pages/MigrationConflictsPage.tsx",
      "packages/app-shared/src/features/information/pages/MigrationDashboardPage.tsx",
      "packages/app-shared/src/features/information/pages/MigrationImportPage.tsx",
      "packages/app-shared/src/features/org/pages/OrgBillingPage.tsx",
      "packages/app-shared/src/hooks/admin/usePermissionsCatalog.ts",
      "packages/app-shared/src/hooks/admin/useUsersAndPermissions.ts",
      "packages/app-shared/src/hooks/information/useMigration.ts",
      "packages/app-shared/src/hooks/lib/parsePermissions.ts",
      "packages/app-shared/src/hooks/permissions/__tests__/useCanAccess.test.tsx",
      "packages/app-shared/src/hooks/permissions/__tests__/usePermission.test.tsx",
      "packages/app-shared/src/hooks/permissions/index.ts",
      "packages/app-shared/src/hooks/permissions/testUtils.tsx",
      "packages/app-shared/src/hooks/permissions/useAbility.ts",
      "packages/app-shared/src/hooks/permissions/useCanAccess.ts",
      "packages/app-shared/src/hooks/permissions/usePermission.ts",
      "packages/app-shared/src/hooks/useOrgBillingApi.ts",
      "packages/app-shared/src/lib/__tests__/permissions.test.ts",
      "packages/app-shared/src/lib/permissions.ts",
      "packages/app-shared/src/lib/personalTokensApi.ts",
      "packages/app-shared/src/pages/AuthLogoutPage.tsx",
      "packages/app-shared/src/pages/platform/AuthAnalyticsPage.impl.tsx",
      "packages/app-shared/src/pages/platform/AuthAnalyticsPage.tsx",
      "packages/app-shared/src/preauth/debug.ts",
      "packages/app-shared/src/preauth/index.ts",
      "packages/app-shared/src/preauth/network.ts",
      "packages/app-shared/src/preauth/session.ts",
      "packages/app-shared/src/preauth/telemetry.ts",
      "packages/app-shared/src/preauth/theme.ts",
      "packages/app-shared/src/preauth/types.ts",
      "packages/app-shared/src/preauth/ui.ts",
      "packages/app-shared/src/preauth/utils.test.ts",
      "packages/app-shared/src/preauth/utils.ts",
      "packages/app-shared/src/router/Unauthorized.tsx",
      "packages/app-shared/src/tests/admin.billing.a11y.test.tsx",
      "packages/app-shared/src/tests/admin.billing.exportmenu.test.tsx",
      "packages/app-shared/src/tests/admin.billing.mobile.test.tsx",
      "packages/app-shared/src/tests/admin.billing.toolbar.smoke.test.tsx",
      "packages/app-shared/src/tests/admin.users.rbac.banner.test.tsx",
      "packages/app-shared/src/tests/api.credentials.test.ts",
      "packages/app-shared/src/tests/auth.can.test.ts",
      "packages/app-shared/src/tests/permission.gate.test.tsx",
      "packages/app-shared/src/tests/router.unauthorized.ui.test.tsx",
      "packages/app-shared/src/tests/suppliers.directory.views.rbac.test.tsx",
      "packages/app-shared/src/types/rbac.ts",
      "packages/auth",
      "packages/auth/package.json",
      "packages/auth/src/__tests__/permissionGrouping.test.ts",
      "packages/auth/src/__tests__/permissionGrouping.test.ts",
      "packages/auth/src/__tests__/rbac-canonical.test.ts",
      "packages/auth/src/__tests__/rbac-canonical.test.ts",
      "packages/auth/src/ability.ts",
      "packages/auth/src/can.ts",
      "packages/auth/src/index.ts",
      "packages/auth/src/logout/broadcast.ts",
      "packages/auth/src/logout/client.ts",
      "packages/auth/src/logout/index.ts",
      "packages/auth/src/permissionGrouping.ts",
      "packages/auth/src/permissionGrouping.ts",
      "packages/auth/src/rbac-canonical.ts",
      "packages/auth/src/rbac-canonical.ts",
      "packages/auth/src/session.ts",
      "packages/auth/test-results/junit.xml",
      "packages/auth/tsconfig.json",
      "packages/auth/tsconfig.tsbuildinfo",
      "packages/config",
      "packages/documentation/migration/page-checklist.json",
      "packages/eslint-plugin-aeptus",
      "packages/types",
      "packages/types/src/rbac.ts",
      "packages/ui",
      "packages/ui/.ai/design-tokens.json",
      "packages/ui/.ai/migration-rules.json",
      "packages/ui/src/components/molecules/TokenPicker/TokenPicker.tsx",
      "packages/ui/src/components/molecules/TokenPicker/index.ts",
      "packages/ui/src/tokens/components.css",
      "packages/ui/src/tokens/index.css",
      "packages/ui/src/tokens/index.ts",
      "packages/ui/src/tokens/primitives.css",
      "packages/ui/src/tokens/semantic.css",
      "packages/ui/src/tokens/themes/dark.css",
      "patches",
      "playwright-report",
      "postgres-18-migration-guide.md",
      "project",
      "public",
      "rbac-cache-delivery.md",
      "rbac-cache-quickstart.md",
      "scripts",
      "scripts/a11y",
      "scripts/adr",
      "scripts/ai",
      "scripts/archive",
      "scripts/assets",
      "scripts/catalog",
      "scripts/checks",
      "scripts/checks/check-customer-preauth-no-design-system.mjs",
      "scripts/ci",
      "scripts/ci/check-endpoint-permissions.mjs",
      "scripts/ci/check-permission-metadata.mjs",
      "scripts/ci/check-route-permissions.sh",
      "scripts/ci/check_migrations.sh",
      "scripts/ci/validate-rbac-sync.mjs",
      "scripts/contracts",
      "scripts/deploy-types.cjs",
      "scripts/deployment/build-production.sh",
      "scripts/design-system",
      "scripts/design-system/generate-token-json.mjs",
      "scripts/dev",
      "scripts/docs",
      "scripts/generate",
      "scripts/help",
      "scripts/i18n",
      "scripts/k6",
      "scripts/maintenance",
      "scripts/migration/audit-page-components.mjs",
      "scripts/naming",
      "scripts/observability",
      "scripts/openapi",
      "scripts/perf",
      "scripts/security",
      "scripts/tools",
      "scripts/trace",
      "scripts/validate-deployment.sh",
      "scripts/validation",
      "scripts/validation/validate_permissions.py",
      "scripts/verify-phase0-deployment.sh",
      "scripts/verify_migration.sh",
      "scripts/ws",
      "shared",
      "src",
      "src/i18n",
      "stories",
      "test-results",
      "tests",
      "tests/contract",
      "tests/contract/consumers/auth.contract.test.ts",
      "tools",
      "tools/mcp-mordor",
      "tools/mcp-mordor/src/tools/rbac.ts"
    ],
    "risks": [
      ".gcloud_access_token",
      ".github/workflows/migrations-guard.yml",
      ".pnpm-store/v10/index/17/3659f9b86de57d0529eeccc33dc3015026947d415796e549a93f9473012b3d-oauth4webapi@3.8.2.json",
      ".pnpm-store/v10/index/18/7b8344ed764b2a6ed9c57bd1dd5d900d845265c7827b6bcdba6f381f90cbee-comma-separated-tokens@1.0.8.json",
      ".pnpm-store/v10/index/29/afbd4ebbadbfb1bc33a593e927a2456cfbf762b9a84a881841b35ca84013ac-class-variance-authority@0.7.1.json",
      ".pnpm-store/v10/index/45/d2547e5704ddc5332a232a420b02bb4e853eef5474824ed1b7986cf8473789-js-tokens@4.0.0.json",
      ".pnpm-store/v10/index/55/dffd1150e2bba3cf26df72021eaba193fa125d711eb76f2151a3c81b074744-@csstools+css-tokenizer@3.0.4.json",
      ".pnpm-store/v10/index/59/dee61cf43ff33cba423edfe13e3abe0ddaa28afc7ec9099ba8366728f4eb8a-@auth+core@0.41.0.json",
      ".pnpm-store/v10/index/9b/16bd13d21314eb746da9f78fa2f93298f07a01b3ea505098cd4826459e0591-js-tokens@9.0.1.json",
      ".pnpm-store/v10/index/a3/69ee27ce43e04491c9b877cdb0390e5d4e7b5edf4592fefd0d7b6f5a90752f-@auth0+auth0-react@2.5.0.json",
      ".pnpm-store/v10/index/ab/f25255dd4ba6dce17f96e4626e286f88963e3c742a245edec44504dad5a9b2-space-separated-tokens@1.1.5.json",
      ".pnpm-store/v10/index/e1/7bf1d84e0dd808abaf5469f8a39e8dd0dba63e4b9df2ed359fd368e768ed56-@auth0+auth0-spa-js@2.5.0.json",
      ".pnpm-store/v10/index/f9/ce7582ab8cdc5ea73159a802eb1127b448a18d0ae13b3d1c20b0cb2fc14687-next-auth@5.0.0-beta.30.json",
      ".pnpm-store/v10/index/ff/b05db84885788349ee695cf22466aa9d2c0f0d9ada50056a18a0fd11a9a67e-eslint-plugin-no-secrets@2.2.1.json",
      ".secrets.baseline",
      "Agents/skills/auth/SKILL.md",
      "Agents/skills/auth/references/api-endpoints.md",
      "Agents/skills/auth/references/api-keys.md",
      "Agents/skills/auth/references/authentication.md",
      "Agents/skills/auth/references/common-patterns.md",
      "Agents/skills/auth/references/database-tables.md",
      "Agents/skills/auth/references/decisions.md",
      "Agents/skills/auth/references/learn-log.md",
      "Agents/skills/auth/references/rbac.md",
      "Agents/skills/auth/references/rbac.md",
      "Agents/skills/auth/references/security.md",
      "Agents/skills/auth/references/troubleshooting.md",
      "Agents/skills/ci-deploy/SKILL.md",
      "Agents/skills/ci-deploy/references/advanced-pipelines.md",
      "Agents/skills/ci-deploy/references/decisions.md",
      "Agents/skills/ci-deploy/references/docker.md",
      "Agents/skills/ci-deploy/references/gcp.md",
      "Agents/skills/ci-deploy/references/kubernetes.md",
      "Agents/skills/ci-deploy/references/learn-log.md",
      "Agents/skills/ci-deploy/references/pipelines.md",
      "Agents/skills/ci-deploy/references/secrets.md",
      "Agents/skills/ci-deploy/references/secrets.md",
      "Agents/skills/database/references/migrations.md",
      "Agents/skills/integrations/references/oauth-flows.md",
      "Agents/tasks/2025-01-13-integrations-onboarding-oauth.md",
      "Agents/tasks/celery-cloudbuild-deploy.md",
      "Agents/tasks/celery-redis-secret-wiring.md",
      "Agents/tasks/dedicated-repo-migration.md",
      "Agents/tasks/fix-bootstrap-permission-case.md",
      "Agents/tasks/fix-environment-discovery-migration.md",
      "Agents/tasks/fix-mordor-roles-permissions-404.md",
      "Agents/tasks/fix-preauth-error-production.md",
      "Agents/tasks/google-oauth-onboarding.md",
      "Agents/tasks/merge-environment-0036-migrations.md",
      "Agents/tasks/otel-step1-deployment.md",
      "Agents/tasks/rbac-implementation-plan-intake.md",
      "Agents/tasks/rbac-pr5-pr8.md",
      "Agents/tasks/rbac-role-management-cleanup.md",
      "Agents/tasks/rbac-role-management-permissions.md",
      "Agents/tasks/role-management-permissions-check.md",
      "apps/customer/src/entry-authenticated.tsx",
      "apps/mordor/src/entry-authenticated.tsx",
      "apps/organizations/src/entry-authenticated.tsx",
      "backend/MIGRATION_SCRIPT.py",
      "backend/accounts/admin_rbac_api_views.py",
      "backend/accounts/admin_rbac_views.py",
      "backend/accounts/auth0_management.py",
      "backend/accounts/auth_analytics_models.py",
      "backend/accounts/auth_analytics_serializers.py",
      "backend/accounts/auth_analytics_views.py",
      "backend/accounts/management/commands/rbac_dump_casl_catalog.py",
      "backend/accounts/management/commands/rbac_lifecycle_tick.py",
      "backend/accounts/management/commands/rbac_roles_summary.py",
      "backend/accounts/management/commands/rbac_seed_permissions.py",
      "backend/accounts/middleware_auth_enforcement.py",
      "backend/accounts/middleware_rbac_identity.py",
      "backend/accounts/migrations/0001_initial.py",
      "backend/accounts/migrations/0002_organization.py",
      "backend/accounts/migrations/0003_userprofile_org_default.py",
      "backend/accounts/migrations/0004_rls_userprofile.py",
      "backend/accounts/migrations/0005_tenant_membership.py",
      "backend/accounts/migrations/0006_userprofile_tenant_nullable.py",
      "backend/accounts/migrations/0007_seed_default_tenants_assign.py",
      "backend/accounts/migrations/0008_userprofile_tenant_nonnull.py",
      "backend/accounts/migrations/0009_rls_userprofile_tenant_update.py",
      "backend/accounts/migrations/0010_alter_userprofile_organization_and_more.py",
      "backend/accounts/migrations/0011_profile_identity_fields.py",
      "backend/accounts/migrations/0012_profile_phone_split.py",
      "backend/accounts/migrations/0013_team_and_identity_extras.py",
      "backend/accounts/migrations/0014_team_id_default.py",
      "backend/accounts/migrations/0015_userprofile_notification_prefs.py",
      "backend/accounts/migrations/0016_userprofile_tz_locale_notif_state.py",
      "backend/accounts/migrations/0017_tenant_notification_policy.py",
      "backend/accounts/migrations/0018_tenant_lifecycle_and_admin_models.py",
      "backend/accounts/migrations/0019_plan_entitlements.py",
      "backend/accounts/migrations/0020_alter_plandefinition_id_and_more.py",
      "backend/accounts/migrations/0021_internal_scopes_and_profile_flag.py",
      "backend/accounts/migrations/0022_custom_attributes.py",
      "backend/accounts/migrations/0023_team_user_custom.py",
      "backend/accounts/migrations/0024_rbac_registry.py",
      "backend/accounts/migrations/0024_rbac_registry.py",
      "backend/accounts/migrations/0025_role_archive.py",
      "backend/accounts/migrations/0025_search_trgm_indexes.py",
      "backend/accounts/migrations/0026_alter_customattributedefinition_id.py",
      "backend/accounts/migrations/0027_merge_20250922_0837.py",
      "backend/accounts/migrations/0028_permission_meta.py",
      "backend/accounts/migrations/0028_permission_meta.py",
      "backend/accounts/migrations/0028_role_risk_fields.py",
      "backend/accounts/migrations/0029_permission_metadata.py",
      "backend/accounts/migrations/0029_permission_metadata.py",
      "backend/accounts/migrations/0030_userprofile_ui_prefs.py",
      "backend/accounts/migrations/0031_enable_tenant_rls.py",
      "backend/accounts/migrations/0032_organization_hierarchy.py",
      "backend/accounts/migrations/0033_remove_organization_org_parent_idx_and_more.py",
      "backend/accounts/migrations/0034_check_constraints.py",
      "backend/accounts/migrations/0035_organization_profile_fields.py",
      "backend/accounts/migrations/0036_grc_organization_fields.py",
      "backend/accounts/migrations/0037_remove_sso_mfa_fields.py",
      "backend/accounts/migrations/0038_alter_organization_tax_id.py",
      "backend/accounts/migrations/0039_tenant_api_calls_month_tenant_api_calls_today_and_more.py",
      "backend/accounts/migrations/0040_tenant_admin_notification_message_and_more.py",
      "backend/accounts/migrations/0041_rolev2_organization_parent_userprofile_primary_team_and_more.py",
      "backend/accounts/migrations/0042_tenanthealthalertrule_tenanthealthmetric_and_more.py",
      "backend/accounts/migrations/0043_broadcasttemplate_scheduledbroadcast_and_more.py",
      "backend/accounts/migrations/0044_alter_scopedpermission_resource_roletemplate.py",
      "backend/accounts/migrations/0044_alter_scopedpermission_resource_roletemplate.py",
      "backend/accounts/migrations/0045_rolev2_tags.py",
      "backend/accounts/migrations/0046_remove_business_unit_and_update_team_types.py",
      "backend/accounts/migrations/0047_sync_rbac_permissions.py",
      "backend/accounts/migrations/0047_sync_rbac_permissions.py",
      "backend/accounts/migrations/0048_remove_userprofile_role.py",
      "backend/accounts/migrations/0049_alter_scopedpermission_resource.py",
      "backend/accounts/migrations/0049_alter_scopedpermission_resource.py",
      "backend/accounts/migrations/0999_rename_rolev2_to_role.py",
      "backend/accounts/migrations/1000_alter_role_options_alter_role_tenant.py",
      "backend/accounts/migrations/1001_drop_legacy_rbac_tables.py",
      "backend/accounts/migrations/1001_drop_legacy_rbac_tables.py",
      "backend/accounts/migrations/1002_recreate_role_permissions_through_table.py",
      "backend/accounts/migrations/1002_recreate_role_permissions_through_table.py",
      "backend/accounts/migrations/1003_alter_scopedpermission_resource.py",
      "backend/accounts/migrations/1003_alter_scopedpermission_resource.py",
      "backend/accounts/migrations/1004_alter_scopedpermission_action.py",
      "backend/accounts/migrations/1004_alter_scopedpermission_action.py",
      "backend/accounts/migrations/1005_add_device_and_session_models.py",
      "backend/accounts/migrations/1006_alter_scopedpermission_resource.py",
      "backend/accounts/migrations/1006_alter_scopedpermission_resource.py",
      "backend/accounts/migrations/1007_add_dashboard_resource.py",
      "backend/accounts/migrations/1008_entitlements_catalog.py",
      "backend/accounts/migrations/1009_seed_owner_internal.py",
      "backend/accounts/migrations/1010_subscription_split.py",
      "backend/accounts/migrations/1011_alter_catalogsubscription_id_alter_creditgrant_id_and_more.py",
      "backend/accounts/migrations/1012_merge_20251105_2056.py",
      "backend/accounts/migrations/1013_delete_rolev2_remove_role_archived_and_more.py",
      "backend/accounts/migrations/1014_notification_columns_and_locale_fields.py",
      "backend/accounts/migrations/1015_merge_20251122_2008.py",
      "backend/accounts/migrations/1016_add_account_models.py",
      "backend/accounts/migrations/1017_assign_demo_admin.py",
      "backend/accounts/migrations/1018_remove_demo_fullaccess_prod.py",
      "backend/accounts/migrations/1019_add_integrations_permissions.py",
      "backend/accounts/migrations/1019_add_integrations_permissions.py",
      "backend/accounts/migrations/1020_add_user_search_trgm_indexes.py",
      "backend/accounts/migrations/1021_role_risk_level_role_risk_meta_and_more.py",
      "backend/accounts/migrations/1022_userprofile_rls_by_user_id.py",
      "backend/accounts/migrations/1023_standardize_rls_gucs.py",
      "backend/accounts/migrations/1024_account_assetentity_fk.py",
      "backend/accounts/migrations/1025_account_roles_permissions_auth.py",
      "backend/accounts/migrations/1025_account_roles_permissions_auth.py",
      "backend/accounts/migrations/1025_account_roles_permissions_auth.py",
      "backend/accounts/migrations/1026_remove_account_accounts_ac_tenant__auth_idx_and_more.py",
      "backend/accounts/migrations/1026_remove_account_accounts_ac_tenant__auth_idx_and_more.py",
      "backend/accounts/migrations/1027_add_risk_rules_permissions.py",
      "backend/accounts/migrations/1027_add_risk_rules_permissions.py",
      "backend/accounts/migrations/1028_add_account_risk_fields.py",
      "backend/accounts/migrations/1029_add_finding_template_model.py",
      "backend/accounts/migrations/1030_role_is_template_role_source_template_and_more.py",
      "backend/accounts/migrations/1031_role_templates_global.py",
      "backend/accounts/migrations/1032_remove_role_accounts_role_template_requires_null_tenant_and_more.py",
      "backend/accounts/migrations/1033_drop_scopedpermission_and_legacy_role_fields.py",
      "backend/accounts/migrations/1033_drop_scopedpermission_and_legacy_role_fields.py",
      "backend/accounts/migrations/1034_alter_userroleassignment_scope_type_and_more.py",
      "backend/accounts/migrations/1035_roleriskpolicy.py",
      "backend/accounts/migrations/1036_account_oauth_scopes.py",
      "backend/accounts/migrations/1036_account_oauth_scopes.py",
      "backend/accounts/migrations/1037_add_external_avatar_url.py",
      "backend/accounts/migrations/1038_grant_demo_admin_v3.py",
      "backend/accounts/migrations/1039_rbac_homogenization.py",
      "backend/accounts/migrations/1039_rbac_homogenization.py",
      "backend/accounts/migrations/1040_remove_permissionauditlog_actor_and_more.py",
      "backend/accounts/migrations/1040_remove_permissionauditlog_actor_and_more.py",
      "backend/accounts/migrations/1041_alter_role_permissions_v3.py",
      "backend/accounts/migrations/1041_alter_role_permissions_v3.py",
      "backend/accounts/migrations/1042_migrate_permissions_to_canonical.py",
      "backend/accounts/migrations/1042_migrate_permissions_to_canonical.py",
      "backend/accounts/migrations/1043_access_grants_and_scope_types.py",
      "backend/accounts/migrations/1044_tenant_slug_global_unique.py",
      "backend/accounts/migrations/1045_rename_accounts_acc_grantor_status_idx_accounts_ac_grantor_970445_idx_and_more.py",
      "backend/accounts/migrations/1046_tenant_onboarding_apps_score_and_more.py",
      "backend/accounts/migrations/1047_tenant_dns_discovery_seed_fields.py",
      "backend/accounts/migrations/1048_authanalyticssummary_authevent.py",
      "backend/accounts/migrations/1048_authanalyticssummary_authevent.py",
      "backend/accounts/migrations/1048_seed_free_plan.py",
      "backend/accounts/migrations/1049_add_domain_role_exposure.py",
      "backend/accounts/migrations/1050_change_domain_role_to_roles_array.py",
      "backend/accounts/migrations/1051_tenant_profiles.py",
      "backend/accounts/migrations/1052_seed_tenant_profiles.py",
      "backend/accounts/migrations/1053_tenant_profile_templates.py",
      "backend/accounts/migrations/1054_seed_tenant_profile_templates.py",
      "backend/accounts/migrations/1055_role_templates_scope_and_profiles.py",
      "backend/accounts/migrations/1056_alter_role_organization_and_more.py",
      "backend/accounts/migrations/1057_tenantdomain_asset_entity.py",
      "backend/accounts/migrations/1058_role_template_visibility_and_auto_create.py",
      "backend/accounts/migrations/1059_fix_account_asset_fk_constraint.py",
      "backend/accounts/migrations/1060_enforce_userprofile_rls.py",
      "backend/accounts/migrations/1061_external_groups.py",
      "backend/accounts/migrations/1062_rename_accounts_ex_tenant__3a632a_idx_accounts_ex_tenant__0c1f4d_idx_and_more.py",
      "backend/accounts/migrations/1063_role_is_platform_staff.py",
      "backend/accounts/migrations/1064_platform_roles.py",
      "backend/accounts/migrations/1065_usersession_realm_enforcement.py",
      "backend/accounts/migrations/1066_remove_platformroleassignment_platform_role_assignment_user_role_uniq_and_more.py",
      "backend/accounts/migrations/1067_consolidate_data_models.py",
      "backend/accounts/migrations/1068_alter_organization_options_alter_team_options_and_more.py",
      "backend/accounts/migrations/1069_documentslot_and_status.py",
      "backend/accounts/migrations/1070_platform_role_assignment_starts_at.py",
      "backend/accounts/migrations/1071_merge_20260202_1350.py",
      "backend/accounts/migrations/1072_seed_default_platform_roles.py",
      "backend/accounts/migrations/1073_feature_key_allow_dots.py",
      "backend/accounts/migrations/1074_aeptus_support_access.py",
      "backend/accounts/migrations/1075_alter_usertenantmembership_role.py",
      "backend/accounts/migrations/1076_userprofile_rls_insert_policy.py",
      "backend/accounts/migrations/1077_migrate_platform_roles_to_canonical_rbac.py",
      "backend/accounts/migrations/1077_migrate_platform_roles_to_canonical_rbac.py",
      "backend/accounts/migrations/1078_userprofile_rls_include_memberships.py",
      "backend/accounts/migrations/1079_userprofile_archived_at.py",
      "backend/accounts/migrations/1080_profile_integrity_jobs.py",
      "backend/accounts/migrations/1081_impersonation_ticket_and_request_id.py",
      "backend/accounts/migrations/1082_alter_tenant_options.py",
      "backend/accounts/migrations/1083_alter_scheduledbroadcast_status.py",
      "backend/accounts/migrations/1084_tenant_profile_fk_and_framework_template.py",
      "backend/accounts/migrations/1085_seed_baseline_framework_templates.py",
      "backend/accounts/migrations/1086_merge_20260305_1932.py",
      "backend/accounts/migrations/FOLDER.migrations.md",
      "backend/accounts/migrations/__init__.py",
      "backend/accounts/permissions_base.py",
      "backend/accounts/rbac.py",
      "backend/accounts/rbac_audit_models.py",
      "backend/accounts/rbac_canonical.py",
      "backend/accounts/rbac_helpers.py",
      "backend/accounts/rbac_models.py",
      "backend/accounts/rbac_permissions.py",
      "backend/accounts/rbac_scope.py",
      "backend/accounts/rbac_signals.py",
      "backend/accounts/tests/test_rbac_access_engine.py",
      "backend/accounts/tests/test_rbac_lifecycle_tick.py",
      "backend/accounts/tests/test_rbac_on_behalf_audit.py",
      "backend/accounts/tests/test_rbac_team_auto_assign.py",
      "backend/adn/migrations/0001_initial.py",
      "backend/adn/migrations/0002_enable_rls.py",
      "backend/adn/migrations/0003_fix_category_slug_uniqueness.py",
      "backend/adn/migrations/0004_pipelinerun_enrichmentqueue_directorysignal_and_more.py",
      "backend/adn/migrations/0005_localproviderentry_localserviceentry_and_more.py",
      "backend/adn/migrations/0006_remove_localproviderentry_unique_local_provider_domain_per_tenant_and_more.py",
      "backend/adn/migrations/0007_add_schema_version.py",
      "backend/adn/migrations/0008_directorycategory_expected_at_onboarding.py",
      "backend/adn/migrations/0009_add_app_metadata_facts.py",
      "backend/adn/migrations/0010_expand_fact_types.py",
      "backend/adn/migrations/0011_add_category_owner_fields.py",
      "backend/adn/migrations/0012_pipelinerun_add_adn_onboarding_enrich_stage.py",
      "backend/adn/migrations/0013_category_owner_delegation.py",
      "backend/adn/migrations/0014_pipelinestageconfig.py",
      "backend/adn/migrations/0015_remove_directoryfact_fact_single_target_entity_and_more.py",
      "backend/adn/migrations/0016_sitemap_supply_chain_choice_expansions.py",
      "backend/adn/migrations/0017_rename_enrichmentqueue_pipelinequeue.py",
      "backend/adn/migrations/0018_rename_adn_pipelin_target__70e8a1_idx_adn_pipelin_target__d13f85_idx_and_more.py",
      "backend/adn/migrations/0019_pipelinebatch.py",
      "backend/adn/migrations/0020_rename_adn_pipelin_status_batch_idx_adn_pipelin_status_90c11e_idx_and_more.py",
      "backend/adn/migrations/__init__.py",
      "backend/adn/permissions.py",
      "backend/adn/tests/test_permissions.py",
      "backend/ai_providers/migrations/0001_initial.py",
      "backend/ai_providers/migrations/0002_seed_providers.py",
      "backend/ai_providers/migrations/__init__.py",
      "backend/analytics/migrations/0001_initial.py",
      "backend/analytics/migrations/__init__.py",
      "backend/api_keys/migrations/0001_initial.py",
      "backend/api_keys/migrations/0002_rename_api_keys_tenant__a3f8b1_idx_api_keys_tenant__aa40c3_idx_and_more.py",
      "backend/api_keys/migrations/0003_unique_constraints.py",
      "backend/api_keys/migrations/0004_apikey_user.py",
      "backend/api_keys/migrations/__init__.py",
      "backend/api_usage/migrations/0001_initial.py",
      "backend/api_usage/migrations/0002_enable_rls.py",
      "backend/api_usage/migrations/0003_rename_api_deprec_tenant_status_idx_api_depreca_tenant__60e9d0_idx_and_more.py",
      "backend/api_usage/migrations/0004_brin_indexes.py",
      "backend/api_usage/migrations/0005_merge_20251001_1316.py",
      "backend/api_usage/migrations/0006_standardize_rls_gucs.py",
      "backend/api_usage/migrations/__init__.py",
      "backend/audit/migrations/0001_initial.py",
      "backend/audit/migrations/0002_rls_and_brin.py",
      "backend/audit/migrations/0003_dedup_unique.py",
      "backend/audit/migrations/0003_partition_shadow_table.py",
      "backend/audit/migrations/0004_alter_auditeventv2_options_and_more.py",
      "backend/audit/migrations/0004_audit_export_job.py",
      "backend/audit/migrations/0005_audit_ingest_keys.py",
      "backend/audit/migrations/0005_audit_phase3_enhancements.py",
      "backend/audit/migrations/0006_auditpolicy_legal_hold.py",
      "backend/audit/migrations/0007_auditpolicy_retention_status.py",
      "backend/audit/migrations/0008_auditeventv2_perf_indexes.py",
      "backend/audit/migrations/0009_merge_0004_0008.py",
      "backend/audit/migrations/0010_drop_audit_event_legacy.py",
      "backend/audit/migrations/0011_alter_auditexportjob_id.py",
      "backend/audit/migrations/0012_auditexportjob_expires_at_auditexportjob_format_and_more.py",
      "backend/audit/migrations/0013_standardize_rls_gucs.py",
      "backend/audit/migrations/0014_actor_id_string.py",
      "backend/audit/migrations/FOLDER.migrations.md",
      "backend/audit/migrations/__init__.py",
      "backend/automations/migrations/0001_initial.py",
      "backend/automations/migrations/0002_definition.py",
      "backend/automations/migrations/0003_data_model_rest.py",
      "backend/automations/migrations/0004_run_logs.py",
      "backend/automations/migrations/0005_enable_rls.py",
      "backend/automations/migrations/0006_check_constraints.py",
      "backend/automations/migrations/0007_performance_indexes.py",
      "backend/automations/migrations/0008_brin_indexes.py",
      "backend/automations/migrations/0009_partial_indexes.py",
      "backend/automations/migrations/0010_remove_automationdefinition_auto_def_tenant_status_idx_and_more.py",
      "backend/automations/migrations/0011_merge_20251106_2056.py",
      "backend/automations/migrations/0012_remove_automationdefinition_created_by_and_more.py",
      "backend/automations/migrations/0013_standardize_rls_gucs.py",
      "backend/automations/migrations/0014_eventdeadletter_payload_json.py",
      "backend/automations/migrations/__init__.py",
      "backend/collaboration/migrations/0001_initial.py",
      "backend/collaboration/migrations/0002_review_models.py",
      "backend/collaboration/migrations/0003_rename_collaborati_locatio_idx_collaborati_locatio_8dcb41_idx_and_more.py",
      "backend/collaboration/migrations/__init__.py",
      "backend/community/migrations/0001_initial.py",
      "backend/community/migrations/0002_enable_rls_policies.py",
      "backend/community/migrations/0003_alter_implicitsignal_target_type.py",
      "backend/community/migrations/__init__.py",
      "backend/controls/migrations/0001_initial.py",
      "backend/controls/migrations/0002_performance_indexes.py",
      "backend/controls/migrations/0003_custom_dashboards.py",
      "backend/controls/migrations/0004_remove_controldefinition_ctrl_tenant_status_domain_idx_and_more.py",
      "backend/controls/migrations/0005_add_control_assessment_items.py",
      "backend/controls/migrations/0006_add_scope_dsl_fields.py",
      "backend/controls/migrations/0007_rename_ctrl_item_tenant_occ_idx_controls_co_tenant__9e7728_idx_and_more.py",
      "backend/controls/migrations/0008_access_review_fields.py",
      "backend/controls/migrations/0009_item_validity_fields.py",
      "backend/controls/migrations/0009_rename_controls_co_tenant_c_kind_idx_controls_co_tenant__9e629c_idx_and_more.py",
      "backend/controls/migrations/0010_merge_20251007_1220.py",
      "backend/controls/migrations/0010_occurrence_signoff_fields.py",
      "backend/controls/migrations/0011_merge_20251007_1252.py",
      "backend/controls/migrations/0012_rename_controls_occ_signoff_due_idx_controls_co_signoff_7bb034_idx.py",
      "backend/controls/migrations/0013_controldefinition_business_unit_and_more.py",
      "backend/controls/migrations/0014_add_composite_indexes.py",
      "backend/controls/migrations/0014_add_performance_indexes.py",
      "backend/controls/migrations/0015_merge_20251104_0914.py",
      "backend/controls/migrations/0016_evidence_and_more.py",
      "backend/controls/migrations/0017_add_search_vector_control.py",
      "backend/controls/migrations/0018_controldefinition_idx_control_search.py",
      "backend/controls/migrations/0019_controldefinition_idx_control_search.py",
      "backend/controls/migrations/0020_enable_rls.py",
      "backend/controls/migrations/0021_evidence_artifact_scan_fields.py",
      "backend/controls/migrations/0022_framework_requirement_and_policy_mapping.py",
      "backend/controls/migrations/0023_populate_framework_requirements.py",
      "backend/controls/migrations/0024_rename_controls_fr_framewo_idx_cat_controls_fr_framewo_83042f_idx_and_more.py",
      "backend/controls/migrations/__init__.py",
      "backend/controls/permissions.py",
      "backend/controls/tests/test_permissions.py",
      "backend/controls/tests/test_rbac_boundary.py",
      "backend/core/management/commands/create_search_permissions.py",
      "backend/core/migrations/0001_initial.py",
      "backend/core/migrations/0003_inapp_security_evidence.py",
      "backend/core/migrations/0004_alerts_evidence_meta_url.py",
      "backend/core/migrations/0005_auditevent_healthcheck_and_more.py",
      "backend/core/migrations/0006_outbound_email_job.py",
      "backend/core/migrations/0007_emailjob_partial_idx.py",
      "backend/core/migrations/0008_outbound_email_bodyhash_unique.py",
      "backend/core/migrations/0009_remove_outboundemailjob_core_emailjob_triplet_uniq_and_more.py",
      "backend/core/migrations/0010_pg_stat_statements_extension.py",
      "backend/core/migrations/0011_delete_auditevent.py",
      "backend/core/migrations/0012_drop_core_auditevent.py",
      "backend/core/migrations/0013_rlsauditevent.py",
      "backend/core/migrations/0014_designsystempage_designsystemcomponent_and_more.py",
      "backend/core/migrations/0015_add_planned_components.py",
      "backend/core/migrations/0016_add_resource_permission_models.py",
      "backend/core/migrations/0016_add_resource_permission_models.py",
      "backend/core/migrations/0017_rlsauditevent.py",
      "backend/core/migrations/0018_change_default_visibility_to_tenant.py",
      "backend/core/migrations/0019_tenantattribute_moduleattributeconfig_and_more.py",
      "backend/core/migrations/0020_queryperformancelog_queryperformancestats.py",
      "backend/core/migrations/0021_rename_core_queryp_created_af2bd6_idx_core_queryp_created_ff0917_idx_and_more.py",
      "backend/core/migrations/0022_merge_20251106_2056.py",
      "backend/core/migrations/0023_search_analytics_models.py",
      "backend/core/migrations/0024_alter_searchanalytics_created_by_and_more.py",
      "backend/core/migrations/0025_export_job.py",
      "backend/core/migrations/0026_enable_rls_core_export_job.py",
      "backend/core/migrations/0027_alter_exportjob_format_alter_exportjob_status.py",
      "backend/core/migrations/FOLDER.migrations.md",
      "backend/core/migrations/__init__.py",
      "backend/core/permissions.py",
      "backend/core/permissions/__init__.py",
      "backend/core/permissions/decorators.py",
      "backend/core/permissions/helpers.py",
      "backend/core/permissions/policy.py",
      "backend/core/permissions/rbac.py",
      "backend/core/permissions/rls_queryset_manager.py",
      "backend/core/permissions/test_utils.py",
      "backend/core/permissions/tests/__init__.py",
      "backend/core/permissions/tests/test_decorators.py",
      "backend/core/permissions/tests/test_helpers.py",
      "backend/core/permissions/tests/test_policy.py",
      "backend/core/tests/test_search/test_search_permissions.py",
      "backend/deployment-guide.md",
      "backend/directory/migrations/0001_initial.py",
      "backend/directory/migrations/0002_bitemporal_constraints.py",
      "backend/directory/migrations/0003_add_service_offering_technology.py",
      "backend/directory/migrations/0004_bitemporal_exclusion_constraints.py",
      "backend/directory/migrations/0005_check_constraints.py",
      "backend/directory/migrations/0006_alter_technologycomponent_unique_together_and_more.py",
      "backend/directory/migrations/0007_service_offering_technology_constraints_and_cleanup.py",
      "backend/directory/migrations/0008_legalentity_categories_serviceoffering_categories_and_more.py",
      "backend/directory/migrations/0009_legalentity_serviceoffering_expansion.py",
      "backend/directory/migrations/0010_legalentity_industry_sanctions_jurisdiction.py",
      "backend/directory/migrations/0011_allow_null_legal_entity_on_service_offering.py",
      "backend/directory/migrations/0012_remove_technologycomponent_categories_and_more.py",
      "backend/directory/migrations/0013_fix_techcat_null_distinct.py",
      "backend/directory/migrations/0014_remove_serviceoffering_tags.py",
      "backend/directory/migrations/0015_technologyproduct_categories.py",
      "backend/directory/migrations/0016_technologycategory_is_active.py",
      "backend/directory/migrations/__init__.py",
      "backend/documents/deployment-guide.md",
      "backend/documents/migrations/0001_initial.py",
      "backend/documents/migrations/0002_rename_doc_t_type_deleted_idx_documents_d_tenant__5ef7dd_idx_and_more.py",
      "backend/documents/migrations/0003_enable_rls.py",
      "backend/documents/migrations/0004_expand_doctype_and_relations.py",
      "backend/documents/migrations/0005_documentslot_and_status.py",
      "backend/documents/migrations/0006_documenttypeprofile.py",
      "backend/documents/migrations/__init__.py",
      "backend/environment/migrations/0001_initial.py",
      "backend/environment/migrations/0002_add_owned_resource_mixin.py",
      "backend/environment/migrations/0002_initial.py",
      "backend/environment/migrations/0002_riskrule_riskrulefielddefinition_riskruleexecution.py",
      "backend/environment/migrations/0003_add_business_security_ownership.py",
      "backend/environment/migrations/0004_alter_asset_visibility_and_more.py",
      "backend/environment/migrations/0005_remove_asset_criticality_asset_service_asset_tier.py",
      "backend/environment/migrations/0006_add_composite_indexes.py",
      "backend/environment/migrations/0006_add_performance_indexes.py",
      "backend/environment/migrations/0007_merge_20251104_0914.py",
      "backend/environment/migrations/0008_remove_asset_env_asset_lifecycle_risk_idx_and_more.py",
      "backend/environment/migrations/0009_merge_20251106_2056.py",
      "backend/environment/migrations/0010_remove_asset_environment_owner_t_925258_idx_and_more.py",
      "backend/environment/migrations/0011_asset_idx_asset_type_tier_stat_and_more.py",
      "backend/environment/migrations/0012_add_search_vector_asset.py",
      "backend/environment/migrations/0013_asset_idx_asset_search.py",
      "backend/environment/migrations/0014_asset_managed_by_thirdpartyentity.py",
      "backend/environment/migrations/0015_alter_asset_unique_together_and_more.py",
      "backend/environment/migrations/0017_bitemporal_table_maintenance_tuning.py",
      "backend/environment/migrations/0018_asset_constraints_and_assettechnology_pair_unique.py",
      "backend/environment/migrations/0019_enable_rls.py",
      "backend/environment/migrations/0020_standardize_risk_fields.py",
      "backend/environment/migrations/0021_merge_20251216_1225.py",
      "backend/environment/migrations/0022_rename_env_riskrule_tenant_target_idx_environment_tenant__8ca752_idx_and_more.py",
      "backend/environment/migrations/0023_riskrulefielddefinition_category.py",
      "backend/environment/migrations/0024_add_asset_risk_breakdown.py",
      "backend/environment/migrations/0025_sprint14_asset_enhancements.py",
      "backend/environment/migrations/0026_rename_env_compmap_t_stat_idx_environment_tenant__24f617_idx_and_more.py",
      "backend/environment/migrations/0026_update_asset_search_vector_business_unit_option.py",
      "backend/environment/migrations/0027_merge_20251224_0905.py",
      "backend/environment/migrations/0028_asset_hosting_model_asset_local_service_and_more.py",
      "backend/environment/migrations/0029_asset_data_model_v12_1.py",
      "backend/environment/migrations/0030_remove_asset_idx_asset_category_and_more.py",
      "backend/environment/migrations/0031_risk_rule_library.py",
      "backend/environment/migrations/0032_remove_orgrulevisibility_unique_org_rule_visibility_and_more.py",
      "backend/environment/migrations/0033_add_asset_domain_registration_fields.py",
      "backend/environment/migrations/0034_alter_asset_asset_type.py",
      "backend/environment/migrations/0035_domain_analyzer_integration_sprint16.py",
      "backend/environment/migrations/0036_asset_discovery_tracking_fields.py",
      "backend/environment/migrations/0036_rename_idx_certhistory_asset_environment_tenant__3daa83_idx_and_more.py",
      "backend/environment/migrations/0037_merge_20260109_0700.py",
      "backend/environment/migrations/0038_threat_intelligence_traffic_ranking.py",
      "backend/environment/migrations/0039_remove_asset_idx_asset_threat_malicious_and_more.py",
      "backend/environment/migrations/0040_technology_fingerprinting.py",
      "backend/environment/migrations/0041_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
      "backend/environment/migrations/0042_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
      "backend/environment/migrations/0043_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
      "backend/environment/migrations/0044_asset_discovery_sources.py",
      "backend/environment/migrations/0045_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
      "backend/environment/migrations/0046_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
      "backend/environment/migrations/0047_update_threatintelligencecheck_constraint.py",
      "backend/environment/migrations/0048_backfill_asset_discovery_sources.py",
      "backend/environment/migrations/0049_asset_directory_category.py",
      "backend/environment/migrations/0050_remove_technologycategory_parent_and_more.py",
      "backend/environment/migrations/0051_remove_asset_business_function_id_and_more.py",
      "backend/environment/migrations/__init__.py",
      "backend/environment/permissions.py",
      "backend/environment/risk_rule_library_permissions.py",
      "backend/environment/risk_rule_permissions.py",
      "backend/events/migrations/0001_initial.py",
      "backend/events/migrations/0002_add_owned_resource_mixin.py",
      "backend/events/migrations/0003_add_business_security_ownership.py",
      "backend/events/migrations/0004_alter_event_visibility_and_more.py",
      "backend/events/migrations/0005_incident.py",
      "backend/events/migrations/0006_delete_incident.py",
      "backend/events/migrations/0007_incident_assetentity.py",
      "backend/events/migrations/0008_alter_incident_created_at_alter_incident_created_by_and_more.py",
      "backend/events/migrations/0009_alter_incident_business_owner_team_and_more.py",
      "backend/events/migrations/__init__.py",
      "backend/frameworks/migrations/__init__.py",
      "backend/gcp/deploy.sh",
      "backend/gcp/setup-search-infrastructure.sh",
      "backend/guide-migrations.md",
      "backend/information/migrations/0001_initial.py",
      "backend/information/migrations/0001_initial_ims_models.py",
      "backend/information/migrations/0002_alter_document_content_type_and_more.py",
      "backend/information/migrations/0003_merge_20251106_2056.py",
      "backend/information/migrations/0004_assetentity_fks_and_contenttypes.py",
      "backend/information/migrations/0005_alter_privacyprofile_content_type.py",
      "backend/information/migrations/__init__.py",
      "backend/information/models/migration.py",
      "backend/information/serializers/migration.py",
      "backend/integrations/migrations/0001_initial.py",
      "backend/integrations/migrations/0002_alter_integrationconnection_provider.py",
      "backend/integrations/migrations/0003_slackinstall.py",
      "backend/integrations/migrations/0004_rename_integrations_slack_tenant_team_idx_integration_tenant__f6ace6_idx.py",
      "backend/integrations/migrations/0005_enhance_integrationconnection_for_adapter_pattern.py",
      "backend/integrations/migrations/0006_enhance_integration_connection.py",
      "backend/integrations/migrations/0007_integrationfieldmapping.py",
      "backend/integrations/migrations/0008_scaling_architecture.py",
      "backend/integrations/migrations/0009_alter_integrationprovider_options_and_more.py",
      "backend/integrations/migrations/0010_integrationaction_integrationdatapoint.py",
      "backend/integrations/migrations/0011_seed_google_workspace_actions_complete.py",
      "backend/integrations/migrations/0012_integrationaction_category.py",
      "backend/integrations/migrations/0013_seed_google_workspace_webhooks.py",
      "backend/integrations/migrations/0014_seed_slack_provider_and_actions.py",
      "backend/integrations/migrations/0015_seed_github_provider_and_actions.py",
      "backend/integrations/migrations/0016_add_is_automation_enabled.py",
      "backend/integrations/migrations/0017_integrationaction_integration_auto_en_idx_and_more.py",
      "backend/integrations/migrations/0018_integration_sync_history.py",
      "backend/integrations/migrations/0019_add_sync_history_data_snapshots.py",
      "backend/integrations/migrations/0020_add_sync_type_choices.py",
      "backend/integrations/migrations/0021_rename_nango_connection_id.py",
      "backend/integrations/migrations/0022_normalize_integration_provider_categories.py",
      "backend/integrations/migrations/0023_seed_microsoft_365_provider.py",
      "backend/integrations/migrations/0024_seed_microsoft_teams_provider.py",
      "backend/integrations/migrations/0025_normalize_connected_status_to_active.py",
      "backend/integrations/migrations/0026_seed_google_workspace_provider.py",
      "backend/integrations/migrations/__init__.py",
      "backend/integrations/tests/test_token_lifecycle_guardrails.py",
      "backend/integrations/tests/test_token_refresh.py",
      "backend/integrations/token-lifecycle-standard.md",
      "backend/knowledge/migrations/0001_initial.py",
      "backend/knowledge/migrations/0002_remove_controlmapping_unique_policy_requirement_and_more.py",
      "backend/knowledge/migrations/__init__.py",
      "backend/localization/migrations/0001_initial.py",
      "backend/localization/migrations/0002_add_owned_resource_mixin.py",
      "backend/localization/migrations/0003_add_business_security_ownership.py",
      "backend/localization/migrations/0004_alter_glossaryterm_visibility_and_more.py",
      "backend/localization/migrations/0005_add_analytics_models.py",
      "backend/localization/migrations/0006_alter_translationchangelog_created_by_and_more.py",
      "backend/localization/migrations/0007_translation_ai_config.py",
      "backend/localization/migrations/__init__.py",
      "backend/manual-deploy-with-verify.sh",
      "backend/mapping_intelligence/migrations/0001_initial.py",
      "backend/mapping_intelligence/migrations/0002_add_missing_fields.py",
      "backend/mapping_intelligence/migrations/0002_fielddefinition_mapping_int_synonym_49b140_gin.py",
      "backend/mapping_intelligence/migrations/0003_add_aimachinesettings.py",
      "backend/mapping_intelligence/migrations/0003_fielddefinition_tenant_scope.py",
      "backend/mapping_intelligence/migrations/0003_rename_mapping_int_entity__idx_mapping_int_entity__9ab5a0_idx_and_more.py",
      "backend/mapping_intelligence/migrations/0004_merge_20251020_1449.py",
      "backend/mapping_intelligence/migrations/0005_add_versioning_and_constraints.py",
      "backend/mapping_intelligence/migrations/0007_add_performance_indexes.py",
      "backend/mapping_intelligence/migrations/0008_merge_20251106_2056.py",
      "backend/mapping_intelligence/migrations/0009_mappinghistory_updated_at_mappinghistory_updated_by_and_more.py",
      "backend/mapping_intelligence/migrations/0010_merge_20260105_1105.py",
      "backend/mapping_intelligence/migrations/0011_remove_fielddefinition_mapping_int_entity__030d87_idx_and_more.py",
      "backend/mapping_intelligence/migrations/__init__.py",
      "backend/mapping_intelligence/permissions.py",
      "backend/menu_overrides/migrations/0001_initial.py",
      "backend/menu_overrides/migrations/0002_add_navigation_analytics.py",
      "backend/menu_overrides/migrations/__init__.py",
      "backend/middleware/rbac_enforcement.py",
      "backend/onboarding/migrations/0001_initial.py",
      "backend/onboarding/migrations/0002_onboardingruntimestate.py",
      "backend/onboarding/migrations/__init__.py",
      "backend/operational/migrations/0001_initial.py",
      "backend/operational/migrations/0002_event_sourcing_triggers.py",
      "backend/operational/migrations/0003_fix_event_sourcing_trigger.py",
      "backend/operational/migrations/0004_trigger_request_id.py",
      "backend/operational/migrations/0005_trigger_request_id_metadata.py",
      "backend/operational/migrations/0006_trigger_update_merge_guard.py",
      "backend/operational/migrations/0007_trigger_merge_guard_jsonb.py",
      "backend/operational/migrations/__init__.py",
      "backend/ops/scripts/deploy-celery-jobs.sh",
      "backend/ops/scripts/deploy-rbac-seed.sh",
      "backend/ops/scripts/deploy-rbac-seed.sh",
      "backend/ops/scripts/execute-rbac-seed.sh",
      "backend/ops/scripts/run-migrations.sh",
      "backend/ops/scripts/seed-rbac-permissions.sh",
      "backend/page_actions/migrations/0001_initial.py",
      "backend/page_actions/migrations/0002_remove_customaction_unique_custom_action_per_org_page_and_more.py",
      "backend/page_actions/migrations/0003_standardize_rls_gucs.py",
      "backend/page_actions/migrations/__init__.py",
      "backend/page_actions/permissions.py",
      "backend/page_actions/services/permission_service.py",
      "backend/page_actions/tests/test_permission_service.py",
      "backend/posture/finding_template_library_permissions.py",
      "backend/posture/migrations/0001_initial.py",
      "backend/posture/migrations/0002_add_owned_resource_mixin.py",
      "backend/posture/migrations/0003_add_business_security_ownership.py",
      "backend/posture/migrations/0004_alter_campaign_visibility_alter_finding_visibility_and_more.py",
      "backend/posture/migrations/0005_add_search_vector_finding.py",
      "backend/posture/migrations/0006_finding_idx_finding_search.py",
      "backend/posture/migrations/0007_alter_campaign_scope_assets_alter_finding_asset_and_more.py",
      "backend/posture/migrations/0008_add_finding_likelihood_and_targets.py",
      "backend/posture/migrations/0009_add_finding_template_model.py",
      "backend/posture/migrations/0010_finding_template_library.py",
      "backend/posture/migrations/0011_seed_finding_template_library.py",
      "backend/posture/migrations/0012_rename_posture_ftl_category_status_idx_posture_ftl_cat_status_idx_and_more.py",
      "backend/posture/migrations/__init__.py",
      "backend/run_migrations.py",
      "backend/scripts/audit_rbac_migration.py",
      "backend/scripts/audit_rbac_migration.py",
      "backend/scripts/debug/test_automations_permission_debug.py",
      "backend/scripts/debug/test_rbac_migration.py",
      "backend/scripts/debug/test_rbac_migration.py",
      "backend/setup-auto-deploy.sh",
      "backend/tasks/migrations/0001_initial.py",
      "backend/tasks/migrations/0002_tasklink.py",
      "backend/tasks/migrations/0003_tasksavedview.py",
      "backend/tasks/migrations/0004_task_tags.py",
      "backend/tasks/migrations/0005_task_comments_watchers.py",
      "backend/tasks/migrations/0006_task_attachment.py",
      "backend/tasks/migrations/0007_checklist_item.py",
      "backend/tasks/migrations/0008_alter_checklistitem_created_at_and_more.py",
      "backend/tasks/migrations/0008_task_workflow_sla.py",
      "backend/tasks/migrations/0009_task_provenance.py",
      "backend/tasks/migrations/0010_merge_20251006_1220.py",
      "backend/tasks/migrations/0011_add_owned_resource_mixin.py",
      "backend/tasks/migrations/0016_task_completion_rule.py",
      "backend/tasks/migrations/0017_add_business_security_ownership.py",
      "backend/tasks/migrations/0018_alter_checklistitem_visibility_alter_task_visibility_and_more.py",
      "backend/tasks/migrations/0019_add_search_vector_task.py",
      "backend/tasks/migrations/0020_task_idx_task_search.py",
      "backend/tasks/migrations/0021_enable_rls.py",
      "backend/tasks/migrations/0022_add_task_decisions.py",
      "backend/tasks/migrations/0023_convert_task_decision_to_task_type.py",
      "backend/tasks/migrations/0024_enforce_single_pending_task_decision.py",
      "backend/tasks/migrations/0025_alter_task_status_alter_task_type.py",
      "backend/tasks/migrations/__init__.py",
      "backend/tests/admin/test_admin_notifications_policy_rbac.py",
      "backend/tests/admin/test_admin_permission_audit.py",
      "backend/tests/admin/test_admin_permission_audit_correlation.py",
      "backend/tests/admin/test_admin_roles_rbac_edit_allow.py",
      "backend/tests/admin/test_admin_roles_rbac_edit_deny.py",
      "backend/tests/admin/test_admin_users_rbac_allow.py",
      "backend/tests/audit/test_audit_events_rbac_deny_audit.py",
      "backend/tests/audit/test_audit_export_rbac_deny.py",
      "backend/tests/audit/test_audit_export_rbac_superuser.py",
      "backend/tests/audit/test_audit_rbac.py",
      "backend/tests/audit/test_audit_rbac_endpoints.py",
      "backend/tests/audit/test_audit_registry_billing_vendor_required.py",
      "backend/tests/audit/test_audit_registry_permissions_required.py",
      "backend/tests/critical/test_auth_oidc.py",
      "backend/tests/integration/test_auth0_idp_asset_bootstrap.py",
      "backend/tests/integration/test_collaboration_authorization.py",
      "backend/tests/integration/test_db_viewer_rbac_allow.py",
      "backend/tests/integration/test_schema_permissions.py",
      "backend/tests/integration/test_schema_ui_permissions.py",
      "backend/tests/integration/test_secret_hashing.py",
      "backend/tests/integration/test_teams_rbac.py",
      "backend/tests/integration/test_teams_rbac_allow.py",
      "backend/tests/integration/test_thirdparty_authorization.py",
      "backend/tests/integration/test_thirdparty_relationship_authorization.py",
      "backend/tests/security/test_auth_flows_comprehensive.py",
      "backend/tests/security/test_auth_login_ratelimit.py",
      "backend/tests/security/test_auth_logout_csrf.py",
      "backend/tests/security/test_auth_session.py",
      "backend/tests/security/test_impersonation_rbac.py",
      "backend/tests/security/test_rbac_admin_api.py",
      "backend/tests/security/test_rbac_casl_mapping.py",
      "backend/tests/security/test_rbac_forbidden_json.py",
      "backend/tests/security/test_rbac_forbidden_json_shape.py",
      "backend/tests/security/test_rbac_risk_matrix.py",
      "backend/tests/security/test_rbac_risk_recalc_command.py",
      "backend/tests/security/test_rbac_settings_guard.py",
      "backend/tests/security/test_thirdparty_unauth_endpoints.py",
      "backend/tests/suppliers/test_suppliers_reports_rbac.py",
      "backend/thirdparties/migrations/0001_initial.py",
      "backend/thirdparties/migrations/0002_enable_rls_policies.py",
      "backend/thirdparties/migrations/0003_bitemporal_constraints.py",
      "backend/thirdparties/migrations/0004_rename_tables_suppliers_to_thirdparties.py",
      "backend/thirdparties/migrations/0005_rename_thirdparties_a_tenant__c63003_idx_thirdpartie_tenant__0c77fd_idx_and_more.py",
      "backend/thirdparties/migrations/0006_rename_thirdparties_a_tenant__c63003_idx_thirdpartie_tenant__0c77fd_idx_and_more.py",
      "backend/thirdparties/migrations/0007_add_asset_service_offering.py",
      "backend/thirdparties/migrations/0008_rename_thirdparties_a_tenant__c63003_idx_thirdpartie_tenant__0c77fd_idx_and_more.py",
      "backend/thirdparties/migrations/0009_supplier_graph_view.py",
      "backend/thirdparties/migrations/0010_add_missing_fields.py",
      "backend/thirdparties/migrations/0011_enable_rls.py",
      "backend/thirdparties/migrations/0012_remove_thirdpartyrelationship_thirdparty_rel_unique_and_more.py",
      "backend/thirdparties/migrations/0012_suppliers_saved_view.py",
      "backend/thirdparties/migrations/0013_asset_service_offering_constraints.py",
      "backend/thirdparties/migrations/0014_check_constraints.py",
      "backend/thirdparties/migrations/0015_performance_indexes.py",
      "backend/thirdparties/migrations/0016_partial_indexes.py",
      "backend/thirdparties/migrations/0017_add_frontend_aligned_fields.py",
      "backend/thirdparties/migrations/0018_add_gdpr_data_privacy_models.py",
      "backend/thirdparties/migrations/0019_remove_dataprivacycontact_created_by_and_more.py",
      "backend/thirdparties/migrations/0020_add_privacy_enhancements.py",
      "backend/thirdparties/migrations/0021_remove_dataprivacycontact_created_by_and_more.py",
      "backend/thirdparties/migrations/0022_repair_privacy_columns.py",
      "backend/thirdparties/migrations/0023_remove_asset_asset_tenant_type_idx_and_more.py",
      "backend/thirdparties/migrations/0024_remove_asset_asset_tenant_type_idx_and_more.py",
      "backend/thirdparties/migrations/0025_alter_document_content_type_and_more.py",
      "backend/thirdparties/migrations/0026_supplierassessment_supplierchangerequest_and_more.py",
      "backend/thirdparties/migrations/0027_dataprivacycontact_dataprivacyprofile_and_more.py",
      "backend/thirdparties/migrations/0028_merge_20251023_1923.py",
      "backend/thirdparties/migrations/0029_tprm_policy_owner_team_doc_source.py",
      "backend/thirdparties/migrations/0030_rename_tp_tenant_owner_user_idx_thirdpartie_tenant__4ef8e4_idx_and_more.py",
      "backend/thirdparties/migrations/0031_add_business_security_ownership.py",
      "backend/thirdparties/migrations/0032_dataprivacycontact_dataprivacyprofile_and_more.py",
      "backend/thirdparties/migrations/0033_alter_thirdparty_visibility.py",
      "backend/thirdparties/migrations/0034_alter_thirdparty_relationship_types.py",
      "backend/thirdparties/migrations/0035_thirdparty_frameworks_alter_thirdparty_tags.py",
      "backend/thirdparties/migrations/0036_alter_thirdparty_tags.py",
      "backend/thirdparties/migrations/0037_add_composite_indexes.py",
      "backend/thirdparties/migrations/0037_add_performance_indexes.py",
      "backend/thirdparties/migrations/0038_merge_20251104_0914.py",
      "backend/thirdparties/migrations/0039_remove_directorylinkconfig_tp_link_sync_idx_and_more.py",
      "backend/thirdparties/migrations/0041_search_extensions_and_indexes.py",
      "backend/thirdparties/migrations/0042_add_search_vector_thirdparty.py",
      "backend/thirdparties/migrations/0043_thirdparty_idx_thirdparty_search.py",
      "backend/thirdparties/migrations/0044_thirdparty_entity_versioning.py",
      "backend/thirdparties/migrations/0045_dataprivacyprofile_third_party_entity.py",
      "backend/thirdparties/migrations/0046_rename_thirdparty_tenant_entity_idx_thirdpartie_tenant__205199_idx_and_more.py",
      "backend/thirdparties/migrations/0047_standardize_rls_gucs.py",
      "backend/thirdparties/migrations/0048_alter_directorylinkconfig_linked_legal_entity_and_more.py",
      "backend/thirdparties/migrations/0049_fix_thirdparty_no_overlap_valid_to_infinity.py",
      "backend/thirdparties/migrations/0050_bitemporal_table_maintenance_tuning.py",
      "backend/thirdparties/migrations/0051_standardize_risk_fields.py",
      "backend/thirdparties/migrations/0052_alter_thirdparty_risk_factors_and_more.py",
      "backend/thirdparties/migrations/0053_directorylinkconfig_linked_local_provider.py",
      "backend/thirdparties/migrations/0054_alter_thirdparty_lifecycle_status.py",
      "backend/thirdparties/migrations/0055_thirdparty_adn_parity_fields.py",
      "backend/thirdparties/migrations/0056_functionalrole_industrycodecrosswalk_and_more.py",
      "backend/thirdparties/migrations/0057_seed_functional_roles.py",
      "backend/thirdparties/migrations/0058_seed_industry_crosswalk.py",
      "backend/thirdparties/migrations/0059_thirdparty_adn_parity_fields.py",
      "backend/thirdparties/migrations/0060_supplier_directory_category.py",
      "backend/thirdparties/migrations/0061_thirdparty_control_frameworks.py",
      "backend/thirdparties/migrations/0062_migrate_frameworks_m2m.py",
      "backend/thirdparties/migrations/0063_alter_thirdparty_frameworks.py",
      "backend/thirdparties/migrations/FOLDER.migrations.md",
      "backend/thirdparties/migrations/__init__.py",
      "backend/webhooks/migrations/0001_initial.py",
      "backend/webhooks/migrations/0002_rename_webhooks_de_subscri_f5d8c1_idx_webhooks_de_subscri_f97236_idx_and_more.py",
      "backend/webhooks/migrations/0003_unique_constraints.py",
      "backend/webhooks/migrations/0004_add_owned_resource_mixin.py",
      "backend/webhooks/migrations/0005_add_business_security_ownership.py",
      "backend/webhooks/migrations/0006_alter_webhookdelivery_visibility_and_more.py",
      "backend/webhooks/migrations/0007_hash_webhook_secrets.py",
      "backend/webhooks/migrations/0007_hash_webhook_secrets.py",
      "backend/webhooks/migrations/0008_alter_webhooksubscription_secret.py",
      "backend/webhooks/migrations/0008_alter_webhooksubscription_secret.py",
      "backend/webhooks/migrations/__init__.py",
      "contracts/migration-to-schemathesis.md",
      "devops/grafana/dashboards/rbac-dashboard.json",
      "devops/prometheus/rules/rbac-alerts.yml",
      "docs/access-auth.md",
      "docs/agents/context/admin.billing.md",
      "docs/api/rbac-api-reference.md",
      "docs/api/rbac-api.md",
      "docs/api/rbac-openapi.json",
      "docs/api/rbac-quick-reference.md",
      "docs/architecture/architecture-deployment.md",
      "docs/architecture/rbac-architecture.md",
      "docs/architecture/security-audit-rbac.md",
      "docs/design-system/automated-deployment-setup.md",
      "docs/design-system/design-tokens-tier-guide.md",
      "docs/feature-flags/catalog-key-migration.md",
      "docs/feature-specs/admin/page-actions/09-deployment-guide.md",
      "docs/feature-specs/controls/deployment-checklist.md",
      "docs/feature-specs/information/my-environment-rbac-integration.md",
      "docs/feature-specs/rbac/admin-roles-review-and-cleanup.md",
      "docs/feature-specs/rbac/rbac-spec.md",
      "docs/feature-specs/search-deployment-guide.md",
      "docs/guides/authentication-setup.md",
      "docs/guides/cost-optimized-deployment.md",
      "docs/guides/deployment-guide-permissions.md",
      "docs/guides/deployment-guide-permissions.md",
      "docs/guides/multi-tenant-deployment-critical.md",
      "docs/guides/post-deployment-setup.md",
      "docs/guides/post-deployment-verification.md",
      "docs/guides/rbac-admin-guide.md",
      "docs/permissions.md",
      "docs/plans/infrastructure-options-comparison.md",
      "docs/prd/rbac-simplified-design.md",
      "docs/rbac-cache-implementation.md",
      "docs/reference/rbac.yaml",
      "docs/reference/reference-rbac-permission-sync.md",
      "docs/runbooks/deploy-admin.md",
      "docs/runbooks/deployment-checklist.md",
      "docs/runbooks/rbac-operations-runbook.md",
      "docs/runbooks/rbac-risk-policy.md",
      "docs/runbooks/runbook-deployment-best-practices.md",
      "docs/runbooks/runbook-production-deployment.md",
      "docs/secret-management-plan.md",
      "docs/security/rbac-risk-policy.md",
      "e2e/fixtures/auth.fixture.ts",
      "e2e/page-objects/auth/login.page.ts",
      "e2e/tests/auth/authentication.spec.ts",
      "manual-deployment-steps.md",
      "migration-complete.md",
      "migration-status.md",
      "migrations-applied-success.md",
      "packages/app-shared/src/app/AuthenticatedApp.tsx",
      "packages/app-shared/src/auth/AbilityContext.shared.ts",
      "packages/app-shared/src/auth/AbilityProvider.ts",
      "packages/app-shared/src/auth/AbilityProviderRoot.tsx",
      "packages/app-shared/src/auth/AuthError.tsx",
      "packages/app-shared/src/auth/FOLDER.auth.md",
      "packages/app-shared/src/auth/NoTenantAccess.tsx",
      "packages/app-shared/src/auth/SessionExpiryWarningProvider.tsx",
      "packages/app-shared/src/auth/SessionGate.tsx",
      "packages/app-shared/src/auth/UseEnvironmentPermissions.tsx",
      "packages/app-shared/src/auth/UseEnvironmentPermissions.tsx",
      "packages/app-shared/src/auth/__tests__/FOLDER.__tests__.md",
      "packages/app-shared/src/auth/__tests__/permissionGrouping.test.ts",
      "packages/app-shared/src/auth/__tests__/permissionGrouping.test.ts",
      "packages/app-shared/src/auth/ability.ts",
      "packages/app-shared/src/auth/can.ts",
      "packages/app-shared/src/auth/logoutBroadcast.ts",
      "packages/app-shared/src/auth/logoutClient.ts",
      "packages/app-shared/src/auth/permissionGrouping.ts",
      "packages/app-shared/src/auth/permissionGrouping.ts",
      "packages/app-shared/src/auth/rbac-canonical.ts",
      "packages/app-shared/src/auth/rbac-canonical.ts",
      "packages/app-shared/src/auth/session.ts",
      "packages/app-shared/src/auth/sessionExpiryWarningContext.ts",
      "packages/app-shared/src/auth/useSessionHeartbeat.ts",
      "packages/app-shared/src/components/admin/AdminBillingView.tsx",
      "packages/app-shared/src/components/admin/OrgBillingOverviewView.tsx",
      "packages/app-shared/src/components/admin/TenantBillingTab.tsx",
      "packages/app-shared/src/components/admin/roles/BatchPermissionUpdates.tsx",
      "packages/app-shared/src/components/admin/roles/PermissionConflictDetector.tsx",
      "packages/app-shared/src/components/admin/roles/PermissionMatrix.tsx",
      "packages/app-shared/src/components/admin/roles/PermissionMatrixSkeleton.tsx",
      "packages/app-shared/src/components/admin/roles/PermissionsList.tsx",
      "packages/app-shared/src/components/admin/roles/__tests__/BatchPermissionUpdates.test.tsx",
      "packages/app-shared/src/components/auth/PermissionDenied.tsx",
      "packages/app-shared/src/components/auth/PermissionDenied.tsx",
      "packages/app-shared/src/constants/rbac-module-settings.md",
      "packages/app-shared/src/constants/rbac.ts",
      "packages/app-shared/src/features/admin/components/AdminBillingView.tsx",
      "packages/app-shared/src/features/admin/components/roles/PermissionConflictDetector.tsx",
      "packages/app-shared/src/features/admin/components/roles/PermissionMatrix.tsx",
      "packages/app-shared/src/features/admin/components/roles/PermissionMatrixSkeleton.tsx",
      "packages/app-shared/src/features/admin/components/roles/permissionConflictRules.ts",
      "packages/app-shared/src/features/admin/components/roles/permissionMatrix.shared.ts",
      "packages/app-shared/src/features/admin/hooks/useUsersAndPermissions.ts",
      "packages/app-shared/src/features/admin/pages/AdminBillingPage.tsx",
      "packages/app-shared/src/features/admin/pages/AdminMigrationDashboardPage.tsx",
      "packages/app-shared/src/features/admin/pages/AdminMigrationDashboardPageView.tsx",
      "packages/app-shared/src/features/auth/components/PermissionDenied.tsx",
      "packages/app-shared/src/features/auth/components/PermissionDenied.tsx",
      "packages/app-shared/src/features/auth/components/index.ts",
      "packages/app-shared/src/features/auth/index.ts",
      "packages/app-shared/src/features/auth/utils/ability.ts",
      "packages/app-shared/src/features/auth/utils/can.ts",
      "packages/app-shared/src/features/auth/utils/index.ts",
      "packages/app-shared/src/features/auth/utils/permissionGrouping.ts",
      "packages/app-shared/src/features/auth/utils/permissionGrouping.ts",
      "packages/app-shared/src/features/auth/utils/session.ts",
      "packages/app-shared/src/features/information/hooks/useMigration.ts",
      "packages/app-shared/src/features/information/pages/MigrationConflictsPage.tsx",
      "packages/app-shared/src/features/information/pages/MigrationDashboardPage.tsx",
      "packages/app-shared/src/features/information/pages/MigrationImportPage.tsx",
      "packages/app-shared/src/features/org/pages/OrgBillingPage.tsx",
      "packages/app-shared/src/hooks/admin/usePermissionsCatalog.ts",
      "packages/app-shared/src/hooks/admin/useUsersAndPermissions.ts",
      "packages/app-shared/src/hooks/information/useMigration.ts",
      "packages/app-shared/src/hooks/lib/parsePermissions.ts",
      "packages/app-shared/src/hooks/permissions/__tests__/useCanAccess.test.tsx",
      "packages/app-shared/src/hooks/permissions/__tests__/usePermission.test.tsx",
      "packages/app-shared/src/hooks/permissions/index.ts",
      "packages/app-shared/src/hooks/permissions/testUtils.tsx",
      "packages/app-shared/src/hooks/permissions/useAbility.ts",
      "packages/app-shared/src/hooks/permissions/useCanAccess.ts",
      "packages/app-shared/src/hooks/permissions/usePermission.ts",
      "packages/app-shared/src/hooks/useOrgBillingApi.ts",
      "packages/app-shared/src/lib/__tests__/permissions.test.ts",
      "packages/app-shared/src/lib/permissions.ts",
      "packages/app-shared/src/lib/personalTokensApi.ts",
      "packages/app-shared/src/pages/AuthLogoutPage.tsx",
      "packages/app-shared/src/pages/platform/AuthAnalyticsPage.impl.tsx",
      "packages/app-shared/src/pages/platform/AuthAnalyticsPage.tsx",
      "packages/app-shared/src/preauth/debug.ts",
      "packages/app-shared/src/preauth/index.ts",
      "packages/app-shared/src/preauth/network.ts",
      "packages/app-shared/src/preauth/session.ts",
      "packages/app-shared/src/preauth/telemetry.ts",
      "packages/app-shared/src/preauth/theme.ts",
      "packages/app-shared/src/preauth/types.ts",
      "packages/app-shared/src/preauth/ui.ts",
      "packages/app-shared/src/preauth/utils.test.ts",
      "packages/app-shared/src/preauth/utils.ts",
      "packages/app-shared/src/router/Unauthorized.tsx",
      "packages/app-shared/src/tests/admin.billing.a11y.test.tsx",
      "packages/app-shared/src/tests/admin.billing.exportmenu.test.tsx",
      "packages/app-shared/src/tests/admin.billing.mobile.test.tsx",
      "packages/app-shared/src/tests/admin.billing.toolbar.smoke.test.tsx",
      "packages/app-shared/src/tests/admin.users.rbac.banner.test.tsx",
      "packages/app-shared/src/tests/api.credentials.test.ts",
      "packages/app-shared/src/tests/auth.can.test.ts",
      "packages/app-shared/src/tests/permission.gate.test.tsx",
      "packages/app-shared/src/tests/router.unauthorized.ui.test.tsx",
      "packages/app-shared/src/tests/suppliers.directory.views.rbac.test.tsx",
      "packages/app-shared/src/types/rbac.ts",
      "packages/auth/package.json",
      "packages/auth/src/__tests__/permissionGrouping.test.ts",
      "packages/auth/src/__tests__/permissionGrouping.test.ts",
      "packages/auth/src/__tests__/rbac-canonical.test.ts",
      "packages/auth/src/__tests__/rbac-canonical.test.ts",
      "packages/auth/src/ability.ts",
      "packages/auth/src/can.ts",
      "packages/auth/src/index.ts",
      "packages/auth/src/logout/broadcast.ts",
      "packages/auth/src/logout/client.ts",
      "packages/auth/src/logout/index.ts",
      "packages/auth/src/permissionGrouping.ts",
      "packages/auth/src/permissionGrouping.ts",
      "packages/auth/src/rbac-canonical.ts",
      "packages/auth/src/rbac-canonical.ts",
      "packages/auth/src/session.ts",
      "packages/auth/test-results/junit.xml",
      "packages/auth/tsconfig.json",
      "packages/auth/tsconfig.tsbuildinfo",
      "packages/documentation/migration/page-checklist.json",
      "packages/types/src/rbac.ts",
      "packages/ui/.ai/design-tokens.json",
      "packages/ui/.ai/migration-rules.json",
      "packages/ui/src/components/molecules/TokenPicker/TokenPicker.tsx",
      "packages/ui/src/components/molecules/TokenPicker/index.ts",
      "packages/ui/src/tokens/components.css",
      "packages/ui/src/tokens/index.css",
      "packages/ui/src/tokens/index.ts",
      "packages/ui/src/tokens/primitives.css",
      "packages/ui/src/tokens/semantic.css",
      "packages/ui/src/tokens/themes/dark.css",
      "postgres-18-migration-guide.md",
      "rbac-cache-delivery.md",
      "rbac-cache-quickstart.md",
      "scripts/checks/check-customer-preauth-no-design-system.mjs",
      "scripts/ci/check-endpoint-permissions.mjs",
      "scripts/ci/check-permission-metadata.mjs",
      "scripts/ci/check-route-permissions.sh",
      "scripts/ci/check_migrations.sh",
      "scripts/ci/validate-rbac-sync.mjs",
      "scripts/deploy-types.cjs",
      "scripts/deployment/build-production.sh",
      "scripts/design-system/generate-token-json.mjs",
      "scripts/migration/audit-page-components.mjs",
      "scripts/validate-deployment.sh",
      "scripts/validation/validate_permissions.py",
      "scripts/verify-phase0-deployment.sh",
      "scripts/verify_migration.sh",
      "tests/contract/consumers/auth.contract.test.ts",
      "tools/mcp-mordor/src/tools/rbac.ts"
    ]
  },
  "task_pack": {
    "task": {
      "raw": "Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.",
      "normalized": "find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.",
      "kind": "navigate_config_ownership"
    },
    "summary": {
      "snapshot": {
        "languages": [
          "javascript",
          "python",
          "typescript"
        ],
        "top_level_dirs": [
          ".chau7",
          ".chunk-history",
          ".claude",
          ".gcloud_tmp",
          ".githooks",
          ".github",
          ".husky",
          ".hypothesis",
          ".lighthouseci",
          ".playwright-mcp",
          ".pnpm-store",
          ".storybook",
          ".wrangler",
          "Agents",
          "TODO",
          "alerts",
          "apps",
          "backend",
          "catalog",
          "config",
          "contracts",
          "devops",
          "docker",
          "docs",
          "e2e",
          "functions",
          "gcp-run-proxy",
          "grafana-provisioning",
          "load_tests",
          "logs",
          "output",
          "packages",
          "patches",
          "playwright-report",
          "project",
          "public",
          "scripts",
          "shared",
          "src",
          "stories",
          "test-results",
          "tests",
          "tools"
        ],
        "readme_path": "tools/mcp-mordor/README.md"
      },
      "files_count": 106096,
      "functions_count": 12763,
      "classes_count": 3255,
      "docs_count": 1085,
      "configs_count": 79
    },
    "signals": {
      "boundary_clarity": {
        "score": 68,
        "level": "mixed",
        "evidence": [
          "cross-area semantic edges: 41827/264266",
          "source files with area assignment: 5350/5369",
          "generic source file names: 13"
        ]
      },
      "entrypoint_clarity": {
        "score": 100,
        "level": "strong",
        "evidence": [
          "direct code entrypoint edges: 1192",
          "configs with entrypoints: 6",
          "areas with ambiguous entrypoints: 1"
        ]
      },
      "config_hygiene": {
        "score": 21,
        "level": "weak",
        "evidence": [
          "operational configs: 39",
          "linked configs: 39/39",
          "duplicate config families: 26"
        ]
      },
      "hidden_coupling": {
        "score": 23,
        "level": "weak",
        "evidence": [
          "low-confidence semantic edges: 209164/235410",
          "high-confidence semantic edges: 14639/235410",
          "cross-area semantic edges: 30948/235410"
        ]
      },
      "parser_visibility": {
        "score": 87,
        "level": "strong",
        "evidence": [
          "supported source files: 5099/5369",
          "source files with semantic extraction: 3816/5369",
          "total extracted functions/classes: 16018"
        ]
      }
    },
    "overview": {
      "overview_docs": [],
      "code_areas": [],
      "reference_areas": [],
      "subareas": [],
      "entrypoints": [],
      "key_configs": [],
      "representative_code_files": [],
      "representative_docs": []
    },
    "anchors": [
      {
        "kind": "folder",
        "id": ".claude/commands",
        "file": null,
        "reason": "area match"
      }
    ],
    "in_scope": {
      "files": [],
      "symbols": [],
      "areas": [
        {
          "value": ".claude/commands",
          "kind": "area",
          "reason": "primary top-level area"
        }
      ]
    },
    "out_of_scope": {
      "files": [],
      "symbols": [],
      "areas": [
        {
          "value": ".chau7",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": ".chunk-history",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": ".claude",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": ".gcloud_access_token",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": ".gcloud_tmp",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": ".githooks",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": ".github",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": ".github/PULL_REQUEST_TEMPLATE",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": ".github/workflows",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": ".github/workflows/migrations-guard.yml",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": ".husky",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": ".hypothesis",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": ".lighthouseci",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": ".playwright-mcp",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": ".pnpm-store",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": ".pnpm-store/v10/index/17/3659f9b86de57d0529eeccc33dc3015026947d415796e549a93f9473012b3d-oauth4webapi@3.8.2.json",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": ".pnpm-store/v10/index/18/7b8344ed764b2a6ed9c57bd1dd5d900d845265c7827b6bcdba6f381f90cbee-comma-separated-tokens@1.0.8.json",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": ".pnpm-store/v10/index/29/afbd4ebbadbfb1bc33a593e927a2456cfbf762b9a84a881841b35ca84013ac-class-variance-authority@0.7.1.json",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": ".pnpm-store/v10/index/45/d2547e5704ddc5332a232a420b02bb4e853eef5474824ed1b7986cf8473789-js-tokens@4.0.0.json",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": ".pnpm-store/v10/index/55/dffd1150e2bba3cf26df72021eaba193fa125d711eb76f2151a3c81b074744-@csstools+css-tokenizer@3.0.4.json",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": ".pnpm-store/v10/index/59/dee61cf43ff33cba423edfe13e3abe0ddaa28afc7ec9099ba8366728f4eb8a-@auth+core@0.41.0.json",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": ".pnpm-store/v10/index/9b/16bd13d21314eb746da9f78fa2f93298f07a01b3ea505098cd4826459e0591-js-tokens@9.0.1.json",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": ".pnpm-store/v10/index/a3/69ee27ce43e04491c9b877cdb0390e5d4e7b5edf4592fefd0d7b6f5a90752f-@auth0+auth0-react@2.5.0.json",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": ".pnpm-store/v10/index/ab/f25255dd4ba6dce17f96e4626e286f88963e3c742a245edec44504dad5a9b2-space-separated-tokens@1.1.5.json",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": ".pnpm-store/v10/index/e1/7bf1d84e0dd808abaf5469f8a39e8dd0dba63e4b9df2ed359fd368e768ed56-@auth0+auth0-spa-js@2.5.0.json",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": ".pnpm-store/v10/index/f9/ce7582ab8cdc5ea73159a802eb1127b448a18d0ae13b3d1c20b0cb2fc14687-next-auth@5.0.0-beta.30.json",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": ".pnpm-store/v10/index/ff/b05db84885788349ee695cf22466aa9d2c0f0d9ada50056a18a0fd11a9a67e-eslint-plugin-no-secrets@2.2.1.json",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": ".secrets.baseline",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": ".storybook",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": ".wrangler",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "Agents",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "Agents/Skills Manager",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "Agents/skills",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "Agents/skills/auth/SKILL.md",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "Agents/skills/auth/references/api-endpoints.md",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "Agents/skills/auth/references/api-keys.md",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "Agents/skills/auth/references/authentication.md",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "Agents/skills/auth/references/common-patterns.md",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "Agents/skills/auth/references/database-tables.md",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "Agents/skills/auth/references/decisions.md",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "Agents/skills/auth/references/learn-log.md",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "Agents/skills/auth/references/rbac.md",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "Agents/skills/auth/references/rbac.md",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "Agents/skills/auth/references/security.md",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "Agents/skills/auth/references/troubleshooting.md",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "Agents/skills/ci-deploy/SKILL.md",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "Agents/skills/ci-deploy/references/advanced-pipelines.md",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "Agents/skills/ci-deploy/references/decisions.md",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "Agents/skills/ci-deploy/references/docker.md",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "Agents/skills/ci-deploy/references/gcp.md",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "Agents/skills/ci-deploy/references/kubernetes.md",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "Agents/skills/ci-deploy/references/learn-log.md",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "Agents/skills/ci-deploy/references/pipelines.md",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "Agents/skills/ci-deploy/references/secrets.md",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "Agents/skills/ci-deploy/references/secrets.md",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": "Agents/skills/database/references/migrations.md",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "Agents/skills/integrations/references/oauth-flows.md",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "Agents/tasks",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "Agents/tasks/2025-01-13-integrations-onboarding-oauth.md",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "Agents/tasks/celery-cloudbuild-deploy.md",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "Agents/tasks/celery-redis-secret-wiring.md",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": "Agents/tasks/dedicated-repo-migration.md",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "Agents/tasks/fix-bootstrap-permission-case.md",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "Agents/tasks/fix-environment-discovery-migration.md",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "Agents/tasks/fix-mordor-roles-permissions-404.md",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "Agents/tasks/fix-preauth-error-production.md",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "Agents/tasks/google-oauth-onboarding.md",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "Agents/tasks/merge-environment-0036-migrations.md",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "Agents/tasks/otel-step1-deployment.md",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "Agents/tasks/rbac-implementation-plan-intake.md",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "Agents/tasks/rbac-pr5-pr8.md",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "Agents/tasks/rbac-role-management-cleanup.md",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "Agents/tasks/rbac-role-management-permissions.md",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "Agents/tasks/role-management-permissions-check.md",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "TODO",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "alerts",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "apps",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "apps/customer",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "apps/customer/src/entry-authenticated.tsx",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "apps/mordor",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "apps/mordor/src/entry-authenticated.tsx",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "apps/organizations",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "apps/organizations/src/entry-authenticated.tsx",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "backend",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/MIGRATION_SCRIPT.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/accounts/admin_rbac_api_views.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/admin_rbac_views.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/auth0_management.py",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "backend/accounts/auth_analytics_models.py",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "backend/accounts/auth_analytics_serializers.py",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "backend/accounts/auth_analytics_views.py",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "backend/accounts/management/commands/rbac_dump_casl_catalog.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/management/commands/rbac_lifecycle_tick.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/management/commands/rbac_roles_summary.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/management/commands/rbac_seed_permissions.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/middleware_auth_enforcement.py",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "backend/accounts/middleware_rbac_identity.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/migrations/0001_initial.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0002_organization.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0003_userprofile_org_default.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0004_rls_userprofile.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0005_tenant_membership.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0006_userprofile_tenant_nullable.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0007_seed_default_tenants_assign.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0008_userprofile_tenant_nonnull.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0009_rls_userprofile_tenant_update.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0010_alter_userprofile_organization_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0011_profile_identity_fields.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0012_profile_phone_split.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0013_team_and_identity_extras.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0014_team_id_default.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0015_userprofile_notification_prefs.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0016_userprofile_tz_locale_notif_state.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0017_tenant_notification_policy.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0018_tenant_lifecycle_and_admin_models.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0019_plan_entitlements.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0020_alter_plandefinition_id_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0021_internal_scopes_and_profile_flag.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0022_custom_attributes.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0023_team_user_custom.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0024_rbac_registry.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/migrations/0024_rbac_registry.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0025_role_archive.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0025_search_trgm_indexes.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0026_alter_customattributedefinition_id.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0027_merge_20250922_0837.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0028_permission_meta.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/migrations/0028_permission_meta.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0028_role_risk_fields.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0029_permission_metadata.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/migrations/0029_permission_metadata.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0030_userprofile_ui_prefs.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0031_enable_tenant_rls.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0032_organization_hierarchy.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0033_remove_organization_org_parent_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0034_check_constraints.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0035_organization_profile_fields.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0036_grc_organization_fields.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0037_remove_sso_mfa_fields.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0038_alter_organization_tax_id.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0039_tenant_api_calls_month_tenant_api_calls_today_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0040_tenant_admin_notification_message_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0041_rolev2_organization_parent_userprofile_primary_team_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0042_tenanthealthalertrule_tenanthealthmetric_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0043_broadcasttemplate_scheduledbroadcast_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0044_alter_scopedpermission_resource_roletemplate.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/migrations/0044_alter_scopedpermission_resource_roletemplate.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0045_rolev2_tags.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0046_remove_business_unit_and_update_team_types.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0047_sync_rbac_permissions.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/migrations/0047_sync_rbac_permissions.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0048_remove_userprofile_role.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0049_alter_scopedpermission_resource.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/migrations/0049_alter_scopedpermission_resource.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/0999_rename_rolev2_to_role.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1000_alter_role_options_alter_role_tenant.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1001_drop_legacy_rbac_tables.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/migrations/1001_drop_legacy_rbac_tables.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1002_recreate_role_permissions_through_table.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/migrations/1002_recreate_role_permissions_through_table.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1003_alter_scopedpermission_resource.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/migrations/1003_alter_scopedpermission_resource.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1004_alter_scopedpermission_action.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/migrations/1004_alter_scopedpermission_action.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1005_add_device_and_session_models.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1006_alter_scopedpermission_resource.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/migrations/1006_alter_scopedpermission_resource.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1007_add_dashboard_resource.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1008_entitlements_catalog.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1009_seed_owner_internal.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1010_subscription_split.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1011_alter_catalogsubscription_id_alter_creditgrant_id_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1012_merge_20251105_2056.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1013_delete_rolev2_remove_role_archived_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1014_notification_columns_and_locale_fields.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1015_merge_20251122_2008.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1016_add_account_models.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1017_assign_demo_admin.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1018_remove_demo_fullaccess_prod.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1019_add_integrations_permissions.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/migrations/1019_add_integrations_permissions.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1020_add_user_search_trgm_indexes.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1021_role_risk_level_role_risk_meta_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1022_userprofile_rls_by_user_id.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1023_standardize_rls_gucs.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1024_account_assetentity_fk.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1025_account_roles_permissions_auth.py",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "backend/accounts/migrations/1025_account_roles_permissions_auth.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/migrations/1025_account_roles_permissions_auth.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1026_remove_account_accounts_ac_tenant__auth_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "backend/accounts/migrations/1026_remove_account_accounts_ac_tenant__auth_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1027_add_risk_rules_permissions.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/migrations/1027_add_risk_rules_permissions.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1028_add_account_risk_fields.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1029_add_finding_template_model.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1030_role_is_template_role_source_template_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1031_role_templates_global.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1032_remove_role_accounts_role_template_requires_null_tenant_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1033_drop_scopedpermission_and_legacy_role_fields.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/migrations/1033_drop_scopedpermission_and_legacy_role_fields.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1034_alter_userroleassignment_scope_type_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1035_roleriskpolicy.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1036_account_oauth_scopes.py",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "backend/accounts/migrations/1036_account_oauth_scopes.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1037_add_external_avatar_url.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1038_grant_demo_admin_v3.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1039_rbac_homogenization.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/migrations/1039_rbac_homogenization.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1040_remove_permissionauditlog_actor_and_more.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/migrations/1040_remove_permissionauditlog_actor_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1041_alter_role_permissions_v3.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/migrations/1041_alter_role_permissions_v3.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1042_migrate_permissions_to_canonical.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/migrations/1042_migrate_permissions_to_canonical.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1043_access_grants_and_scope_types.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1044_tenant_slug_global_unique.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1045_rename_accounts_acc_grantor_status_idx_accounts_ac_grantor_970445_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1046_tenant_onboarding_apps_score_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1047_tenant_dns_discovery_seed_fields.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1048_authanalyticssummary_authevent.py",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "backend/accounts/migrations/1048_authanalyticssummary_authevent.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1048_seed_free_plan.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1049_add_domain_role_exposure.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1050_change_domain_role_to_roles_array.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1051_tenant_profiles.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1052_seed_tenant_profiles.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1053_tenant_profile_templates.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1054_seed_tenant_profile_templates.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1055_role_templates_scope_and_profiles.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1056_alter_role_organization_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1057_tenantdomain_asset_entity.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1058_role_template_visibility_and_auto_create.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1059_fix_account_asset_fk_constraint.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1060_enforce_userprofile_rls.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1061_external_groups.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1062_rename_accounts_ex_tenant__3a632a_idx_accounts_ex_tenant__0c1f4d_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1063_role_is_platform_staff.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1064_platform_roles.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1065_usersession_realm_enforcement.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1066_remove_platformroleassignment_platform_role_assignment_user_role_uniq_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1067_consolidate_data_models.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1068_alter_organization_options_alter_team_options_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1069_documentslot_and_status.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1070_platform_role_assignment_starts_at.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1071_merge_20260202_1350.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1072_seed_default_platform_roles.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1073_feature_key_allow_dots.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1074_aeptus_support_access.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1075_alter_usertenantmembership_role.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1076_userprofile_rls_insert_policy.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1077_migrate_platform_roles_to_canonical_rbac.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/migrations/1077_migrate_platform_roles_to_canonical_rbac.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1078_userprofile_rls_include_memberships.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1079_userprofile_archived_at.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1080_profile_integrity_jobs.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1081_impersonation_ticket_and_request_id.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1082_alter_tenant_options.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1083_alter_scheduledbroadcast_status.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1084_tenant_profile_fk_and_framework_template.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1085_seed_baseline_framework_templates.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/1086_merge_20260305_1932.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/FOLDER.migrations.md",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/migrations/__init__.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/accounts/permissions_base.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/rbac.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/rbac_audit_models.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/rbac_canonical.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/rbac_helpers.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/rbac_models.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/rbac_permissions.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/rbac_scope.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/rbac_signals.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/tests/test_rbac_access_engine.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/tests/test_rbac_lifecycle_tick.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/tests/test_rbac_on_behalf_audit.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/accounts/tests/test_rbac_team_auto_assign.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/adn",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/adn/migrations/0001_initial.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/adn/migrations/0002_enable_rls.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/adn/migrations/0003_fix_category_slug_uniqueness.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/adn/migrations/0004_pipelinerun_enrichmentqueue_directorysignal_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/adn/migrations/0005_localproviderentry_localserviceentry_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/adn/migrations/0006_remove_localproviderentry_unique_local_provider_domain_per_tenant_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/adn/migrations/0007_add_schema_version.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/adn/migrations/0008_directorycategory_expected_at_onboarding.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/adn/migrations/0009_add_app_metadata_facts.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/adn/migrations/0010_expand_fact_types.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/adn/migrations/0011_add_category_owner_fields.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/adn/migrations/0012_pipelinerun_add_adn_onboarding_enrich_stage.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/adn/migrations/0013_category_owner_delegation.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/adn/migrations/0014_pipelinestageconfig.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/adn/migrations/0015_remove_directoryfact_fact_single_target_entity_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/adn/migrations/0016_sitemap_supply_chain_choice_expansions.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/adn/migrations/0017_rename_enrichmentqueue_pipelinequeue.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/adn/migrations/0018_rename_adn_pipelin_target__70e8a1_idx_adn_pipelin_target__d13f85_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/adn/migrations/0019_pipelinebatch.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/adn/migrations/0020_rename_adn_pipelin_status_batch_idx_adn_pipelin_status_90c11e_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/adn/migrations/__init__.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/adn/permissions.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/adn/tests/test_permissions.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/aep_backend",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/ai_providers",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/ai_providers/migrations/0001_initial.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/ai_providers/migrations/0002_seed_providers.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/ai_providers/migrations/__init__.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/analytics",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/analytics/migrations/0001_initial.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/analytics/migrations/__init__.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/api_keys",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/api_keys/migrations/0001_initial.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/api_keys/migrations/0002_rename_api_keys_tenant__a3f8b1_idx_api_keys_tenant__aa40c3_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/api_keys/migrations/0003_unique_constraints.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/api_keys/migrations/0004_apikey_user.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/api_keys/migrations/__init__.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/api_usage",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/api_usage/migrations/0001_initial.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/api_usage/migrations/0002_enable_rls.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/api_usage/migrations/0003_rename_api_deprec_tenant_status_idx_api_depreca_tenant__60e9d0_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/api_usage/migrations/0004_brin_indexes.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/api_usage/migrations/0005_merge_20251001_1316.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/api_usage/migrations/0006_standardize_rls_gucs.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/api_usage/migrations/__init__.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/audit",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/audit/migrations/0001_initial.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/audit/migrations/0002_rls_and_brin.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/audit/migrations/0003_dedup_unique.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/audit/migrations/0003_partition_shadow_table.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/audit/migrations/0004_alter_auditeventv2_options_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/audit/migrations/0004_audit_export_job.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/audit/migrations/0005_audit_ingest_keys.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/audit/migrations/0005_audit_phase3_enhancements.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/audit/migrations/0006_auditpolicy_legal_hold.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/audit/migrations/0007_auditpolicy_retention_status.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/audit/migrations/0008_auditeventv2_perf_indexes.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/audit/migrations/0009_merge_0004_0008.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/audit/migrations/0010_drop_audit_event_legacy.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/audit/migrations/0011_alter_auditexportjob_id.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/audit/migrations/0012_auditexportjob_expires_at_auditexportjob_format_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/audit/migrations/0013_standardize_rls_gucs.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/audit/migrations/0014_actor_id_string.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/audit/migrations/FOLDER.migrations.md",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/audit/migrations/__init__.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/automations",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/automations/migrations/0001_initial.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/automations/migrations/0002_definition.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/automations/migrations/0003_data_model_rest.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/automations/migrations/0004_run_logs.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/automations/migrations/0005_enable_rls.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/automations/migrations/0006_check_constraints.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/automations/migrations/0007_performance_indexes.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/automations/migrations/0008_brin_indexes.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/automations/migrations/0009_partial_indexes.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/automations/migrations/0010_remove_automationdefinition_auto_def_tenant_status_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/automations/migrations/0011_merge_20251106_2056.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/automations/migrations/0012_remove_automationdefinition_created_by_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/automations/migrations/0013_standardize_rls_gucs.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/automations/migrations/0014_eventdeadletter_payload_json.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/automations/migrations/__init__.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/collaboration",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/collaboration/migrations/0001_initial.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/collaboration/migrations/0002_review_models.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/collaboration/migrations/0003_rename_collaborati_locatio_idx_collaborati_locatio_8dcb41_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/collaboration/migrations/__init__.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/common",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/community",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/community/migrations/0001_initial.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/community/migrations/0002_enable_rls_policies.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/community/migrations/0003_alter_implicitsignal_target_type.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/community/migrations/__init__.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/controls",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/controls/migrations/0001_initial.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/controls/migrations/0002_performance_indexes.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/controls/migrations/0003_custom_dashboards.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/controls/migrations/0004_remove_controldefinition_ctrl_tenant_status_domain_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/controls/migrations/0005_add_control_assessment_items.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/controls/migrations/0006_add_scope_dsl_fields.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/controls/migrations/0007_rename_ctrl_item_tenant_occ_idx_controls_co_tenant__9e7728_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/controls/migrations/0008_access_review_fields.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/controls/migrations/0009_item_validity_fields.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/controls/migrations/0009_rename_controls_co_tenant_c_kind_idx_controls_co_tenant__9e629c_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/controls/migrations/0010_merge_20251007_1220.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/controls/migrations/0010_occurrence_signoff_fields.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/controls/migrations/0011_merge_20251007_1252.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/controls/migrations/0012_rename_controls_occ_signoff_due_idx_controls_co_signoff_7bb034_idx.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/controls/migrations/0013_controldefinition_business_unit_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/controls/migrations/0014_add_composite_indexes.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/controls/migrations/0014_add_performance_indexes.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/controls/migrations/0015_merge_20251104_0914.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/controls/migrations/0016_evidence_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/controls/migrations/0017_add_search_vector_control.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/controls/migrations/0018_controldefinition_idx_control_search.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/controls/migrations/0019_controldefinition_idx_control_search.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/controls/migrations/0020_enable_rls.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/controls/migrations/0021_evidence_artifact_scan_fields.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/controls/migrations/0022_framework_requirement_and_policy_mapping.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/controls/migrations/0023_populate_framework_requirements.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/controls/migrations/0024_rename_controls_fr_framewo_idx_cat_controls_fr_framewo_83042f_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/controls/migrations/__init__.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/controls/permissions.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/controls/tests/test_permissions.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/controls/tests/test_rbac_boundary.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/core",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/core/management/commands/create_search_permissions.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/core/migrations/0001_initial.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/core/migrations/0003_inapp_security_evidence.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/core/migrations/0004_alerts_evidence_meta_url.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/core/migrations/0005_auditevent_healthcheck_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/core/migrations/0006_outbound_email_job.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/core/migrations/0007_emailjob_partial_idx.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/core/migrations/0008_outbound_email_bodyhash_unique.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/core/migrations/0009_remove_outboundemailjob_core_emailjob_triplet_uniq_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/core/migrations/0010_pg_stat_statements_extension.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/core/migrations/0011_delete_auditevent.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/core/migrations/0012_drop_core_auditevent.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/core/migrations/0013_rlsauditevent.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/core/migrations/0014_designsystempage_designsystemcomponent_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/core/migrations/0015_add_planned_components.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/core/migrations/0016_add_resource_permission_models.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/core/migrations/0016_add_resource_permission_models.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/core/migrations/0017_rlsauditevent.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/core/migrations/0018_change_default_visibility_to_tenant.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/core/migrations/0019_tenantattribute_moduleattributeconfig_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/core/migrations/0020_queryperformancelog_queryperformancestats.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/core/migrations/0021_rename_core_queryp_created_af2bd6_idx_core_queryp_created_ff0917_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/core/migrations/0022_merge_20251106_2056.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/core/migrations/0023_search_analytics_models.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/core/migrations/0024_alter_searchanalytics_created_by_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/core/migrations/0025_export_job.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/core/migrations/0026_enable_rls_core_export_job.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/core/migrations/0027_alter_exportjob_format_alter_exportjob_status.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/core/migrations/FOLDER.migrations.md",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/core/migrations/__init__.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/core/permissions.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/core/permissions/__init__.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/core/permissions/decorators.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/core/permissions/helpers.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/core/permissions/policy.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/core/permissions/rbac.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/core/permissions/rls_queryset_manager.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/core/permissions/test_utils.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/core/permissions/tests/__init__.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/core/permissions/tests/test_decorators.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/core/permissions/tests/test_helpers.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/core/permissions/tests/test_policy.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/core/tests/test_search/test_search_permissions.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/deployment-guide.md",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "backend/directory",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/directory/migrations/0001_initial.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/directory/migrations/0002_bitemporal_constraints.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/directory/migrations/0003_add_service_offering_technology.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/directory/migrations/0004_bitemporal_exclusion_constraints.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/directory/migrations/0005_check_constraints.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/directory/migrations/0006_alter_technologycomponent_unique_together_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/directory/migrations/0007_service_offering_technology_constraints_and_cleanup.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/directory/migrations/0008_legalentity_categories_serviceoffering_categories_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/directory/migrations/0009_legalentity_serviceoffering_expansion.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/directory/migrations/0010_legalentity_industry_sanctions_jurisdiction.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/directory/migrations/0011_allow_null_legal_entity_on_service_offering.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/directory/migrations/0012_remove_technologycomponent_categories_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/directory/migrations/0013_fix_techcat_null_distinct.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/directory/migrations/0014_remove_serviceoffering_tags.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/directory/migrations/0015_technologyproduct_categories.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/directory/migrations/0016_technologycategory_is_active.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/directory/migrations/__init__.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/docs",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/documents",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/documents/deployment-guide.md",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "backend/documents/migrations/0001_initial.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/documents/migrations/0002_rename_doc_t_type_deleted_idx_documents_d_tenant__5ef7dd_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/documents/migrations/0003_enable_rls.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/documents/migrations/0004_expand_doctype_and_relations.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/documents/migrations/0005_documentslot_and_status.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/documents/migrations/0006_documenttypeprofile.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/documents/migrations/__init__.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/environment/migrations/0001_initial.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0002_add_owned_resource_mixin.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0002_initial.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0002_riskrule_riskrulefielddefinition_riskruleexecution.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0003_add_business_security_ownership.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0004_alter_asset_visibility_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0005_remove_asset_criticality_asset_service_asset_tier.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0006_add_composite_indexes.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0006_add_performance_indexes.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0007_merge_20251104_0914.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0008_remove_asset_env_asset_lifecycle_risk_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0009_merge_20251106_2056.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0010_remove_asset_environment_owner_t_925258_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0011_asset_idx_asset_type_tier_stat_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0012_add_search_vector_asset.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0013_asset_idx_asset_search.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0014_asset_managed_by_thirdpartyentity.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0015_alter_asset_unique_together_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0017_bitemporal_table_maintenance_tuning.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0018_asset_constraints_and_assettechnology_pair_unique.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0019_enable_rls.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0020_standardize_risk_fields.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0021_merge_20251216_1225.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0022_rename_env_riskrule_tenant_target_idx_environment_tenant__8ca752_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0023_riskrulefielddefinition_category.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0024_add_asset_risk_breakdown.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0025_sprint14_asset_enhancements.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0026_rename_env_compmap_t_stat_idx_environment_tenant__24f617_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0026_update_asset_search_vector_business_unit_option.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0027_merge_20251224_0905.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0028_asset_hosting_model_asset_local_service_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0029_asset_data_model_v12_1.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0030_remove_asset_idx_asset_category_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0031_risk_rule_library.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0032_remove_orgrulevisibility_unique_org_rule_visibility_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0033_add_asset_domain_registration_fields.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0034_alter_asset_asset_type.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0035_domain_analyzer_integration_sprint16.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0036_asset_discovery_tracking_fields.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0036_rename_idx_certhistory_asset_environment_tenant__3daa83_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0037_merge_20260109_0700.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0038_threat_intelligence_traffic_ranking.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0039_remove_asset_idx_asset_threat_malicious_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0040_technology_fingerprinting.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0041_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0042_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0043_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0044_asset_discovery_sources.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0045_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0046_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0047_update_threatintelligencecheck_constraint.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0048_backfill_asset_discovery_sources.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0049_asset_directory_category.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0050_remove_technologycategory_parent_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/0051_remove_asset_business_function_id_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/migrations/__init__.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/environment/permissions.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/environment/risk_rule_library_permissions.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/environment/risk_rule_permissions.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/events",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/events/migrations/0001_initial.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/events/migrations/0002_add_owned_resource_mixin.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/events/migrations/0003_add_business_security_ownership.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/events/migrations/0004_alter_event_visibility_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/events/migrations/0005_incident.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/events/migrations/0006_delete_incident.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/events/migrations/0007_incident_assetentity.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/events/migrations/0008_alter_incident_created_at_alter_incident_created_by_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/events/migrations/0009_alter_incident_business_owner_team_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/events/migrations/__init__.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/frameworks",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/frameworks/migrations/__init__.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/gcp/deploy.sh",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "backend/gcp/setup-search-infrastructure.sh",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "backend/guide-migrations.md",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/information",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/information/migrations/0001_initial.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/information/migrations/0001_initial_ims_models.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/information/migrations/0002_alter_document_content_type_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/information/migrations/0003_merge_20251106_2056.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/information/migrations/0004_assetentity_fks_and_contenttypes.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/information/migrations/0005_alter_privacyprofile_content_type.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/information/migrations/__init__.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/information/models/migration.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/information/serializers/migration.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/integrations",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/integrations/migrations/0001_initial.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/integrations/migrations/0002_alter_integrationconnection_provider.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/integrations/migrations/0003_slackinstall.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/integrations/migrations/0004_rename_integrations_slack_tenant_team_idx_integration_tenant__f6ace6_idx.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/integrations/migrations/0005_enhance_integrationconnection_for_adapter_pattern.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/integrations/migrations/0006_enhance_integration_connection.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/integrations/migrations/0007_integrationfieldmapping.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/integrations/migrations/0008_scaling_architecture.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/integrations/migrations/0009_alter_integrationprovider_options_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/integrations/migrations/0010_integrationaction_integrationdatapoint.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/integrations/migrations/0011_seed_google_workspace_actions_complete.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/integrations/migrations/0012_integrationaction_category.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/integrations/migrations/0013_seed_google_workspace_webhooks.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/integrations/migrations/0014_seed_slack_provider_and_actions.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/integrations/migrations/0015_seed_github_provider_and_actions.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/integrations/migrations/0016_add_is_automation_enabled.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/integrations/migrations/0017_integrationaction_integration_auto_en_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/integrations/migrations/0018_integration_sync_history.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/integrations/migrations/0019_add_sync_history_data_snapshots.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/integrations/migrations/0020_add_sync_type_choices.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/integrations/migrations/0021_rename_nango_connection_id.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/integrations/migrations/0022_normalize_integration_provider_categories.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/integrations/migrations/0023_seed_microsoft_365_provider.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/integrations/migrations/0024_seed_microsoft_teams_provider.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/integrations/migrations/0025_normalize_connected_status_to_active.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/integrations/migrations/0026_seed_google_workspace_provider.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/integrations/migrations/__init__.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/integrations/tests/test_token_lifecycle_guardrails.py",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": "backend/integrations/tests/test_token_refresh.py",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": "backend/integrations/token-lifecycle-standard.md",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": "backend/k8s",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/knowledge",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/knowledge/migrations/0001_initial.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/knowledge/migrations/0002_remove_controlmapping_unique_policy_requirement_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/knowledge/migrations/__init__.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/localization",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/localization/migrations/0001_initial.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/localization/migrations/0002_add_owned_resource_mixin.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/localization/migrations/0003_add_business_security_ownership.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/localization/migrations/0004_alter_glossaryterm_visibility_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/localization/migrations/0005_add_analytics_models.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/localization/migrations/0006_alter_translationchangelog_created_by_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/localization/migrations/0007_translation_ai_config.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/localization/migrations/__init__.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/manual-deploy-with-verify.sh",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "backend/mapping_intelligence",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/mapping_intelligence/migrations/0001_initial.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/mapping_intelligence/migrations/0002_add_missing_fields.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/mapping_intelligence/migrations/0002_fielddefinition_mapping_int_synonym_49b140_gin.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/mapping_intelligence/migrations/0003_add_aimachinesettings.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/mapping_intelligence/migrations/0003_fielddefinition_tenant_scope.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/mapping_intelligence/migrations/0003_rename_mapping_int_entity__idx_mapping_int_entity__9ab5a0_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/mapping_intelligence/migrations/0004_merge_20251020_1449.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/mapping_intelligence/migrations/0005_add_versioning_and_constraints.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/mapping_intelligence/migrations/0007_add_performance_indexes.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/mapping_intelligence/migrations/0008_merge_20251106_2056.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/mapping_intelligence/migrations/0009_mappinghistory_updated_at_mappinghistory_updated_by_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/mapping_intelligence/migrations/0010_merge_20260105_1105.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/mapping_intelligence/migrations/0011_remove_fielddefinition_mapping_int_entity__030d87_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/mapping_intelligence/migrations/__init__.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/mapping_intelligence/permissions.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/menu_overrides",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/menu_overrides/migrations/0001_initial.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/menu_overrides/migrations/0002_add_navigation_analytics.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/menu_overrides/migrations/__init__.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/middleware",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/middleware/rbac_enforcement.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/onboarding",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/onboarding/migrations/0001_initial.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/onboarding/migrations/0002_onboardingruntimestate.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/onboarding/migrations/__init__.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/operational",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/operational/migrations/0001_initial.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/operational/migrations/0002_event_sourcing_triggers.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/operational/migrations/0003_fix_event_sourcing_trigger.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/operational/migrations/0004_trigger_request_id.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/operational/migrations/0005_trigger_request_id_metadata.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/operational/migrations/0006_trigger_update_merge_guard.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/operational/migrations/0007_trigger_merge_guard_jsonb.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/operational/migrations/__init__.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/ops/scripts/deploy-celery-jobs.sh",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "backend/ops/scripts/deploy-rbac-seed.sh",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "backend/ops/scripts/deploy-rbac-seed.sh",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/ops/scripts/execute-rbac-seed.sh",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/ops/scripts/run-migrations.sh",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/ops/scripts/seed-rbac-permissions.sh",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/page_actions",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/page_actions/migrations/0001_initial.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/page_actions/migrations/0002_remove_customaction_unique_custom_action_per_org_page_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/page_actions/migrations/0003_standardize_rls_gucs.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/page_actions/migrations/__init__.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/page_actions/permissions.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/page_actions/services/permission_service.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/page_actions/tests/test_permission_service.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/posture",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/posture/finding_template_library_permissions.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/posture/migrations/0001_initial.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/posture/migrations/0002_add_owned_resource_mixin.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/posture/migrations/0003_add_business_security_ownership.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/posture/migrations/0004_alter_campaign_visibility_alter_finding_visibility_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/posture/migrations/0005_add_search_vector_finding.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/posture/migrations/0006_finding_idx_finding_search.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/posture/migrations/0007_alter_campaign_scope_assets_alter_finding_asset_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/posture/migrations/0008_add_finding_likelihood_and_targets.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/posture/migrations/0009_add_finding_template_model.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/posture/migrations/0010_finding_template_library.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/posture/migrations/0011_seed_finding_template_library.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/posture/migrations/0012_rename_posture_ftl_category_status_idx_posture_ftl_cat_status_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/posture/migrations/__init__.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/project",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/reports",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/run_migrations.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/scripts",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/scripts/audit_rbac_migration.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/scripts/audit_rbac_migration.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/scripts/debug/test_automations_permission_debug.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/scripts/debug/test_rbac_migration.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/scripts/debug/test_rbac_migration.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/setup-auto-deploy.sh",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "backend/tasks",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/tasks/migrations/0001_initial.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/tasks/migrations/0002_tasklink.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/tasks/migrations/0003_tasksavedview.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/tasks/migrations/0004_task_tags.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/tasks/migrations/0005_task_comments_watchers.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/tasks/migrations/0006_task_attachment.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/tasks/migrations/0007_checklist_item.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/tasks/migrations/0008_alter_checklistitem_created_at_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/tasks/migrations/0008_task_workflow_sla.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/tasks/migrations/0009_task_provenance.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/tasks/migrations/0010_merge_20251006_1220.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/tasks/migrations/0011_add_owned_resource_mixin.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/tasks/migrations/0016_task_completion_rule.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/tasks/migrations/0017_add_business_security_ownership.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/tasks/migrations/0018_alter_checklistitem_visibility_alter_task_visibility_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/tasks/migrations/0019_add_search_vector_task.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/tasks/migrations/0020_task_idx_task_search.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/tasks/migrations/0021_enable_rls.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/tasks/migrations/0022_add_task_decisions.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/tasks/migrations/0023_convert_task_decision_to_task_type.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/tasks/migrations/0024_enforce_single_pending_task_decision.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/tasks/migrations/0025_alter_task_status_alter_task_type.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/tasks/migrations/__init__.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/templates",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/test-results",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/tests",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/tests/admin/test_admin_notifications_policy_rbac.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/tests/admin/test_admin_permission_audit.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/tests/admin/test_admin_permission_audit_correlation.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/tests/admin/test_admin_roles_rbac_edit_allow.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/tests/admin/test_admin_roles_rbac_edit_deny.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/tests/admin/test_admin_users_rbac_allow.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/tests/audit/test_audit_events_rbac_deny_audit.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/tests/audit/test_audit_export_rbac_deny.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/tests/audit/test_audit_export_rbac_superuser.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/tests/audit/test_audit_rbac.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/tests/audit/test_audit_rbac_endpoints.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/tests/audit/test_audit_registry_billing_vendor_required.py",
          "kind": "area",
          "reason": "high-risk area: billing logic"
        },
        {
          "value": "backend/tests/audit/test_audit_registry_permissions_required.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/tests/critical/test_auth_oidc.py",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "backend/tests/integration/test_auth0_idp_asset_bootstrap.py",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "backend/tests/integration/test_collaboration_authorization.py",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "backend/tests/integration/test_db_viewer_rbac_allow.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/tests/integration/test_schema_permissions.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/tests/integration/test_schema_ui_permissions.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/tests/integration/test_secret_hashing.py",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": "backend/tests/integration/test_teams_rbac.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/tests/integration/test_teams_rbac_allow.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/tests/integration/test_thirdparty_authorization.py",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "backend/tests/integration/test_thirdparty_relationship_authorization.py",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "backend/tests/security/test_auth_flows_comprehensive.py",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "backend/tests/security/test_auth_login_ratelimit.py",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "backend/tests/security/test_auth_logout_csrf.py",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "backend/tests/security/test_auth_session.py",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "backend/tests/security/test_impersonation_rbac.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/tests/security/test_rbac_admin_api.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/tests/security/test_rbac_casl_mapping.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/tests/security/test_rbac_forbidden_json.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/tests/security/test_rbac_forbidden_json_shape.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/tests/security/test_rbac_risk_matrix.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/tests/security/test_rbac_risk_recalc_command.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/tests/security/test_rbac_settings_guard.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/tests/security/test_thirdparty_unauth_endpoints.py",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "backend/tests/suppliers/test_suppliers_reports_rbac.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "backend/thirdparties",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/thirdparties/migrations/0001_initial.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0002_enable_rls_policies.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0003_bitemporal_constraints.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0004_rename_tables_suppliers_to_thirdparties.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0005_rename_thirdparties_a_tenant__c63003_idx_thirdpartie_tenant__0c77fd_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0006_rename_thirdparties_a_tenant__c63003_idx_thirdpartie_tenant__0c77fd_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0007_add_asset_service_offering.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0008_rename_thirdparties_a_tenant__c63003_idx_thirdpartie_tenant__0c77fd_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0009_supplier_graph_view.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0010_add_missing_fields.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0011_enable_rls.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0012_remove_thirdpartyrelationship_thirdparty_rel_unique_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0012_suppliers_saved_view.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0013_asset_service_offering_constraints.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0014_check_constraints.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0015_performance_indexes.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0016_partial_indexes.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0017_add_frontend_aligned_fields.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0018_add_gdpr_data_privacy_models.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0019_remove_dataprivacycontact_created_by_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0020_add_privacy_enhancements.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0021_remove_dataprivacycontact_created_by_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0022_repair_privacy_columns.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0023_remove_asset_asset_tenant_type_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0024_remove_asset_asset_tenant_type_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0025_alter_document_content_type_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0026_supplierassessment_supplierchangerequest_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0027_dataprivacycontact_dataprivacyprofile_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0028_merge_20251023_1923.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0029_tprm_policy_owner_team_doc_source.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0030_rename_tp_tenant_owner_user_idx_thirdpartie_tenant__4ef8e4_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0031_add_business_security_ownership.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0032_dataprivacycontact_dataprivacyprofile_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0033_alter_thirdparty_visibility.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0034_alter_thirdparty_relationship_types.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0035_thirdparty_frameworks_alter_thirdparty_tags.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0036_alter_thirdparty_tags.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0037_add_composite_indexes.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0037_add_performance_indexes.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0038_merge_20251104_0914.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0039_remove_directorylinkconfig_tp_link_sync_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0041_search_extensions_and_indexes.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0042_add_search_vector_thirdparty.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0043_thirdparty_idx_thirdparty_search.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0044_thirdparty_entity_versioning.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0045_dataprivacyprofile_third_party_entity.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0046_rename_thirdparty_tenant_entity_idx_thirdpartie_tenant__205199_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0047_standardize_rls_gucs.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0048_alter_directorylinkconfig_linked_legal_entity_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0049_fix_thirdparty_no_overlap_valid_to_infinity.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0050_bitemporal_table_maintenance_tuning.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0051_standardize_risk_fields.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0052_alter_thirdparty_risk_factors_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0053_directorylinkconfig_linked_local_provider.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0054_alter_thirdparty_lifecycle_status.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0055_thirdparty_adn_parity_fields.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0056_functionalrole_industrycodecrosswalk_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0057_seed_functional_roles.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0058_seed_industry_crosswalk.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0059_thirdparty_adn_parity_fields.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0060_supplier_directory_category.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0061_thirdparty_control_frameworks.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0062_migrate_frameworks_m2m.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/0063_alter_thirdparty_frameworks.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/FOLDER.migrations.md",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/thirdparties/migrations/__init__.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/webhooks",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/webhooks/migrations/0001_initial.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/webhooks/migrations/0002_rename_webhooks_de_subscri_f5d8c1_idx_webhooks_de_subscri_f97236_idx_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/webhooks/migrations/0003_unique_constraints.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/webhooks/migrations/0004_add_owned_resource_mixin.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/webhooks/migrations/0005_add_business_security_ownership.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/webhooks/migrations/0006_alter_webhookdelivery_visibility_and_more.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/webhooks/migrations/0007_hash_webhook_secrets.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/webhooks/migrations/0007_hash_webhook_secrets.py",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": "backend/webhooks/migrations/0008_alter_webhooksubscription_secret.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "backend/webhooks/migrations/0008_alter_webhooksubscription_secret.py",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": "backend/webhooks/migrations/__init__.py",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "catalog",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "config",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "config/bundle",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "config/lighthouse",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "config/observability",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "config/quality",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "contracts",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "contracts/config",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "contracts/migration-to-schemathesis.md",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "devops",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "devops/grafana/dashboards/rbac-dashboard.json",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "devops/prometheus/rules/rbac-alerts.yml",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "docker",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/access-auth.md",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "docs/adr",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/agents",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/agents/context/admin.billing.md",
          "kind": "area",
          "reason": "high-risk area: billing logic"
        },
        {
          "value": "docs/api",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/api/rbac-api-reference.md",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "docs/api/rbac-api.md",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "docs/api/rbac-openapi.json",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "docs/api/rbac-quick-reference.md",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "docs/architecture",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/architecture/architecture-deployment.md",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "docs/architecture/rbac-architecture.md",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "docs/architecture/security-audit-rbac.md",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "docs/badges",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/collaboration",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/contracts",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/db",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/design-system",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/design-system/automated-deployment-setup.md",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "docs/design-system/design-tokens-tier-guide.md",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": "docs/development",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/docker",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/engineering",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/feature-flags/catalog-key-migration.md",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "docs/feature-specs",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/feature-specs/admin/page-actions/09-deployment-guide.md",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "docs/feature-specs/controls/deployment-checklist.md",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "docs/feature-specs/information/my-environment-rbac-integration.md",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "docs/feature-specs/rbac/admin-roles-review-and-cleanup.md",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "docs/feature-specs/rbac/rbac-spec.md",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "docs/feature-specs/search-deployment-guide.md",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "docs/guides",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/guides/authentication-setup.md",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "docs/guides/cost-optimized-deployment.md",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "docs/guides/deployment-guide-permissions.md",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "docs/guides/deployment-guide-permissions.md",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "docs/guides/multi-tenant-deployment-critical.md",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "docs/guides/post-deployment-setup.md",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "docs/guides/post-deployment-verification.md",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "docs/guides/rbac-admin-guide.md",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "docs/observability",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/onboarding",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/openapi",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/otlp",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/performance",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/permissions.md",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "docs/planning",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/plans",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/plans/infrastructure-options-comparison.md",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "docs/prd",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/prd/rbac-simplified-design.md",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "docs/rbac-cache-implementation.md",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "docs/reference",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/reference/rbac.yaml",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "docs/reference/reference-rbac-permission-sync.md",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "docs/reports",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/runbooks",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/runbooks/deploy-admin.md",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "docs/runbooks/deployment-checklist.md",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "docs/runbooks/rbac-operations-runbook.md",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "docs/runbooks/rbac-risk-policy.md",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "docs/runbooks/runbook-deployment-best-practices.md",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "docs/runbooks/runbook-production-deployment.md",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "docs/secret-management-plan.md",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": "docs/security",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/security/rbac-risk-policy.md",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "docs/testing",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "e2e",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "e2e/fixtures",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "e2e/fixtures/auth.fixture.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "e2e/page-objects",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "e2e/page-objects/auth/login.page.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "e2e/tests/auth/authentication.spec.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "functions",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "gcp-run-proxy",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "gcp-run-proxy/src",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "gcp-run-proxy/test",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "grafana-provisioning",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "load_tests",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "logs",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "manual-deployment-steps.md",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "migration-complete.md",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "migration-status.md",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "migrations-applied-success.md",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "output",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "packages",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "packages/app-shared",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "packages/app-shared/src/app/AuthenticatedApp.tsx",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/auth/AbilityContext.shared.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/auth/AbilityProvider.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/auth/AbilityProviderRoot.tsx",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/auth/AuthError.tsx",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/auth/FOLDER.auth.md",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/auth/NoTenantAccess.tsx",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/auth/SessionExpiryWarningProvider.tsx",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/auth/SessionGate.tsx",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/auth/UseEnvironmentPermissions.tsx",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/auth/UseEnvironmentPermissions.tsx",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/auth/__tests__/FOLDER.__tests__.md",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/auth/__tests__/permissionGrouping.test.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/auth/__tests__/permissionGrouping.test.ts",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/auth/ability.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/auth/can.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/auth/logoutBroadcast.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/auth/logoutClient.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/auth/permissionGrouping.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/auth/permissionGrouping.ts",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/auth/rbac-canonical.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/auth/rbac-canonical.ts",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/auth/session.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/auth/sessionExpiryWarningContext.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/auth/useSessionHeartbeat.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/components/admin/AdminBillingView.tsx",
          "kind": "area",
          "reason": "high-risk area: billing logic"
        },
        {
          "value": "packages/app-shared/src/components/admin/OrgBillingOverviewView.tsx",
          "kind": "area",
          "reason": "high-risk area: billing logic"
        },
        {
          "value": "packages/app-shared/src/components/admin/TenantBillingTab.tsx",
          "kind": "area",
          "reason": "high-risk area: billing logic"
        },
        {
          "value": "packages/app-shared/src/components/admin/roles/BatchPermissionUpdates.tsx",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/components/admin/roles/PermissionConflictDetector.tsx",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/components/admin/roles/PermissionMatrix.tsx",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/components/admin/roles/PermissionMatrixSkeleton.tsx",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/components/admin/roles/PermissionsList.tsx",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/components/admin/roles/__tests__/BatchPermissionUpdates.test.tsx",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/components/auth/PermissionDenied.tsx",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/components/auth/PermissionDenied.tsx",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/constants/rbac-module-settings.md",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/constants/rbac.ts",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/features/admin/components/AdminBillingView.tsx",
          "kind": "area",
          "reason": "high-risk area: billing logic"
        },
        {
          "value": "packages/app-shared/src/features/admin/components/roles/PermissionConflictDetector.tsx",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/features/admin/components/roles/PermissionMatrix.tsx",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/features/admin/components/roles/PermissionMatrixSkeleton.tsx",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/features/admin/components/roles/permissionConflictRules.ts",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/features/admin/components/roles/permissionMatrix.shared.ts",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/features/admin/hooks/useUsersAndPermissions.ts",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/features/admin/pages/AdminBillingPage.tsx",
          "kind": "area",
          "reason": "high-risk area: billing logic"
        },
        {
          "value": "packages/app-shared/src/features/admin/pages/AdminMigrationDashboardPage.tsx",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "packages/app-shared/src/features/admin/pages/AdminMigrationDashboardPageView.tsx",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "packages/app-shared/src/features/auth/components/PermissionDenied.tsx",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/features/auth/components/PermissionDenied.tsx",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/features/auth/components/index.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/features/auth/index.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/features/auth/utils/ability.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/features/auth/utils/can.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/features/auth/utils/index.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/features/auth/utils/permissionGrouping.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/features/auth/utils/permissionGrouping.ts",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/features/auth/utils/session.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/features/information/hooks/useMigration.ts",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "packages/app-shared/src/features/information/pages/MigrationConflictsPage.tsx",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "packages/app-shared/src/features/information/pages/MigrationDashboardPage.tsx",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "packages/app-shared/src/features/information/pages/MigrationImportPage.tsx",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "packages/app-shared/src/features/org/pages/OrgBillingPage.tsx",
          "kind": "area",
          "reason": "high-risk area: billing logic"
        },
        {
          "value": "packages/app-shared/src/hooks/admin/usePermissionsCatalog.ts",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/hooks/admin/useUsersAndPermissions.ts",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/hooks/information/useMigration.ts",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "packages/app-shared/src/hooks/lib/parsePermissions.ts",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/hooks/permissions/__tests__/useCanAccess.test.tsx",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/hooks/permissions/__tests__/usePermission.test.tsx",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/hooks/permissions/index.ts",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/hooks/permissions/testUtils.tsx",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/hooks/permissions/useAbility.ts",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/hooks/permissions/useCanAccess.ts",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/hooks/permissions/usePermission.ts",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/hooks/useOrgBillingApi.ts",
          "kind": "area",
          "reason": "high-risk area: billing logic"
        },
        {
          "value": "packages/app-shared/src/lib/__tests__/permissions.test.ts",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/lib/permissions.ts",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/lib/personalTokensApi.ts",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": "packages/app-shared/src/pages/AuthLogoutPage.tsx",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/pages/platform/AuthAnalyticsPage.impl.tsx",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/pages/platform/AuthAnalyticsPage.tsx",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/preauth/debug.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/preauth/index.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/preauth/network.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/preauth/session.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/preauth/telemetry.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/preauth/theme.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/preauth/types.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/preauth/ui.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/preauth/utils.test.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/preauth/utils.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/router/Unauthorized.tsx",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/tests/admin.billing.a11y.test.tsx",
          "kind": "area",
          "reason": "high-risk area: billing logic"
        },
        {
          "value": "packages/app-shared/src/tests/admin.billing.exportmenu.test.tsx",
          "kind": "area",
          "reason": "high-risk area: billing logic"
        },
        {
          "value": "packages/app-shared/src/tests/admin.billing.mobile.test.tsx",
          "kind": "area",
          "reason": "high-risk area: billing logic"
        },
        {
          "value": "packages/app-shared/src/tests/admin.billing.toolbar.smoke.test.tsx",
          "kind": "area",
          "reason": "high-risk area: billing logic"
        },
        {
          "value": "packages/app-shared/src/tests/admin.users.rbac.banner.test.tsx",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/tests/api.credentials.test.ts",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": "packages/app-shared/src/tests/auth.can.test.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/tests/permission.gate.test.tsx",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/tests/router.unauthorized.ui.test.tsx",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/app-shared/src/tests/suppliers.directory.views.rbac.test.tsx",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/app-shared/src/types/rbac.ts",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/auth",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "packages/auth/package.json",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/auth/src/__tests__/permissionGrouping.test.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/auth/src/__tests__/permissionGrouping.test.ts",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/auth/src/__tests__/rbac-canonical.test.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/auth/src/__tests__/rbac-canonical.test.ts",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/auth/src/ability.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/auth/src/can.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/auth/src/index.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/auth/src/logout/broadcast.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/auth/src/logout/client.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/auth/src/logout/index.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/auth/src/permissionGrouping.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/auth/src/permissionGrouping.ts",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/auth/src/rbac-canonical.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/auth/src/rbac-canonical.ts",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/auth/src/session.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/auth/test-results/junit.xml",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/auth/tsconfig.json",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/auth/tsconfig.tsbuildinfo",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "packages/config",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "packages/documentation/migration/page-checklist.json",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "packages/eslint-plugin-aeptus",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "packages/types",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "packages/types/src/rbac.ts",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "packages/ui",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "packages/ui/.ai/design-tokens.json",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": "packages/ui/.ai/migration-rules.json",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "packages/ui/src/components/molecules/TokenPicker/TokenPicker.tsx",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": "packages/ui/src/components/molecules/TokenPicker/index.ts",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": "packages/ui/src/tokens/components.css",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": "packages/ui/src/tokens/index.css",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": "packages/ui/src/tokens/index.ts",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": "packages/ui/src/tokens/primitives.css",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": "packages/ui/src/tokens/semantic.css",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": "packages/ui/src/tokens/themes/dark.css",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": "patches",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "playwright-report",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "postgres-18-migration-guide.md",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "project",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "public",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "rbac-cache-delivery.md",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "rbac-cache-quickstart.md",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "scripts",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/a11y",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/adr",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/ai",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/archive",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/assets",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/catalog",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/checks",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/checks/check-customer-preauth-no-design-system.mjs",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "scripts/ci",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/ci/check-endpoint-permissions.mjs",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "scripts/ci/check-permission-metadata.mjs",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "scripts/ci/check-route-permissions.sh",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "scripts/ci/check_migrations.sh",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "scripts/ci/validate-rbac-sync.mjs",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "scripts/contracts",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/deploy-types.cjs",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "scripts/deployment/build-production.sh",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "scripts/design-system",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/design-system/generate-token-json.mjs",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": "scripts/dev",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/docs",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/generate",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/help",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/i18n",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/k6",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/maintenance",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/migration/audit-page-components.mjs",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "scripts/naming",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/observability",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/openapi",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/perf",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/security",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/tools",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/trace",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/validate-deployment.sh",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "scripts/validation",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/validation/validate_permissions.py",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        },
        {
          "value": "scripts/verify-phase0-deployment.sh",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "scripts/verify_migration.sh",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "scripts/ws",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "shared",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "src",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "src/i18n",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "stories",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "test-results",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "tests",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "tests/contract",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "tests/contract/consumers/auth.contract.test.ts",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "tools",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "tools/mcp-mordor",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "tools/mcp-mordor/src/tools/rbac.ts",
          "kind": "area",
          "reason": "high-risk area: permission boundary"
        }
      ]
    },
    "dependencies": [],
    "impact": [],
    "snippets": [],
    "risk_flags": [
      {
        "scope": ".gcloud_access_token",
        "area": "secrets",
        "level": "high",
        "reason": "sensitive credential surface"
      },
      {
        "scope": ".github/workflows/migrations-guard.yml",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": ".pnpm-store/v10/index/17/3659f9b86de57d0529eeccc33dc3015026947d415796e549a93f9473012b3d-oauth4webapi@3.8.2.json",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": ".pnpm-store/v10/index/18/7b8344ed764b2a6ed9c57bd1dd5d900d845265c7827b6bcdba6f381f90cbee-comma-separated-tokens@1.0.8.json",
        "area": "secrets",
        "level": "high",
        "reason": "sensitive credential surface"
      },
      {
        "scope": ".pnpm-store/v10/index/29/afbd4ebbadbfb1bc33a593e927a2456cfbf762b9a84a881841b35ca84013ac-class-variance-authority@0.7.1.json",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": ".pnpm-store/v10/index/45/d2547e5704ddc5332a232a420b02bb4e853eef5474824ed1b7986cf8473789-js-tokens@4.0.0.json",
        "area": "secrets",
        "level": "high",
        "reason": "sensitive credential surface"
      },
      {
        "scope": ".pnpm-store/v10/index/55/dffd1150e2bba3cf26df72021eaba193fa125d711eb76f2151a3c81b074744-@csstools+css-tokenizer@3.0.4.json",
        "area": "secrets",
        "level": "high",
        "reason": "sensitive credential surface"
      },
      {
        "scope": ".pnpm-store/v10/index/59/dee61cf43ff33cba423edfe13e3abe0ddaa28afc7ec9099ba8366728f4eb8a-@auth+core@0.41.0.json",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": ".pnpm-store/v10/index/9b/16bd13d21314eb746da9f78fa2f93298f07a01b3ea505098cd4826459e0591-js-tokens@9.0.1.json",
        "area": "secrets",
        "level": "high",
        "reason": "sensitive credential surface"
      },
      {
        "scope": ".pnpm-store/v10/index/a3/69ee27ce43e04491c9b877cdb0390e5d4e7b5edf4592fefd0d7b6f5a90752f-@auth0+auth0-react@2.5.0.json",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": ".pnpm-store/v10/index/ab/f25255dd4ba6dce17f96e4626e286f88963e3c742a245edec44504dad5a9b2-space-separated-tokens@1.1.5.json",
        "area": "secrets",
        "level": "high",
        "reason": "sensitive credential surface"
      },
      {
        "scope": ".pnpm-store/v10/index/e1/7bf1d84e0dd808abaf5469f8a39e8dd0dba63e4b9df2ed359fd368e768ed56-@auth0+auth0-spa-js@2.5.0.json",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": ".pnpm-store/v10/index/f9/ce7582ab8cdc5ea73159a802eb1127b448a18d0ae13b3d1c20b0cb2fc14687-next-auth@5.0.0-beta.30.json",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": ".pnpm-store/v10/index/ff/b05db84885788349ee695cf22466aa9d2c0f0d9ada50056a18a0fd11a9a67e-eslint-plugin-no-secrets@2.2.1.json",
        "area": "secrets",
        "level": "high",
        "reason": "sensitive credential surface"
      },
      {
        "scope": ".secrets.baseline",
        "area": "secrets",
        "level": "high",
        "reason": "sensitive credential surface"
      },
      {
        "scope": "Agents/skills/auth/SKILL.md",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "Agents/skills/auth/references/api-endpoints.md",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "Agents/skills/auth/references/api-keys.md",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "Agents/skills/auth/references/authentication.md",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "Agents/skills/auth/references/common-patterns.md",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "Agents/skills/auth/references/database-tables.md",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "Agents/skills/auth/references/decisions.md",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "Agents/skills/auth/references/learn-log.md",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "Agents/skills/auth/references/rbac.md",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "Agents/skills/auth/references/rbac.md",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "Agents/skills/auth/references/security.md",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "Agents/skills/auth/references/troubleshooting.md",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "Agents/skills/ci-deploy/SKILL.md",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "Agents/skills/ci-deploy/references/advanced-pipelines.md",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "Agents/skills/ci-deploy/references/decisions.md",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "Agents/skills/ci-deploy/references/docker.md",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "Agents/skills/ci-deploy/references/gcp.md",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "Agents/skills/ci-deploy/references/kubernetes.md",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "Agents/skills/ci-deploy/references/learn-log.md",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "Agents/skills/ci-deploy/references/pipelines.md",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "Agents/skills/ci-deploy/references/secrets.md",
        "area": "secrets",
        "level": "high",
        "reason": "sensitive credential surface"
      },
      {
        "scope": "Agents/skills/ci-deploy/references/secrets.md",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "Agents/skills/database/references/migrations.md",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "Agents/skills/integrations/references/oauth-flows.md",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "Agents/tasks/2025-01-13-integrations-onboarding-oauth.md",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "Agents/tasks/celery-cloudbuild-deploy.md",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "Agents/tasks/celery-redis-secret-wiring.md",
        "area": "secrets",
        "level": "high",
        "reason": "sensitive credential surface"
      },
      {
        "scope": "Agents/tasks/dedicated-repo-migration.md",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "Agents/tasks/fix-bootstrap-permission-case.md",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "Agents/tasks/fix-environment-discovery-migration.md",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "Agents/tasks/fix-mordor-roles-permissions-404.md",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "Agents/tasks/fix-preauth-error-production.md",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "Agents/tasks/google-oauth-onboarding.md",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "Agents/tasks/merge-environment-0036-migrations.md",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "Agents/tasks/otel-step1-deployment.md",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "Agents/tasks/rbac-implementation-plan-intake.md",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "Agents/tasks/rbac-pr5-pr8.md",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "Agents/tasks/rbac-role-management-cleanup.md",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "Agents/tasks/rbac-role-management-permissions.md",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "Agents/tasks/role-management-permissions-check.md",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "apps/customer/src/entry-authenticated.tsx",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "apps/mordor/src/entry-authenticated.tsx",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "apps/organizations/src/entry-authenticated.tsx",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "backend/MIGRATION_SCRIPT.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/admin_rbac_api_views.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/admin_rbac_views.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/auth0_management.py",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "backend/accounts/auth_analytics_models.py",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "backend/accounts/auth_analytics_serializers.py",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "backend/accounts/auth_analytics_views.py",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "backend/accounts/management/commands/rbac_dump_casl_catalog.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/management/commands/rbac_lifecycle_tick.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/management/commands/rbac_roles_summary.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/management/commands/rbac_seed_permissions.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/middleware_auth_enforcement.py",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "backend/accounts/middleware_rbac_identity.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/migrations/0001_initial.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0002_organization.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0003_userprofile_org_default.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0004_rls_userprofile.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0005_tenant_membership.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0006_userprofile_tenant_nullable.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0007_seed_default_tenants_assign.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0008_userprofile_tenant_nonnull.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0009_rls_userprofile_tenant_update.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0010_alter_userprofile_organization_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0011_profile_identity_fields.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0012_profile_phone_split.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0013_team_and_identity_extras.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0014_team_id_default.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0015_userprofile_notification_prefs.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0016_userprofile_tz_locale_notif_state.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0017_tenant_notification_policy.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0018_tenant_lifecycle_and_admin_models.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0019_plan_entitlements.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0020_alter_plandefinition_id_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0021_internal_scopes_and_profile_flag.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0022_custom_attributes.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0023_team_user_custom.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0024_rbac_registry.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/migrations/0024_rbac_registry.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0025_role_archive.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0025_search_trgm_indexes.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0026_alter_customattributedefinition_id.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0027_merge_20250922_0837.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0028_permission_meta.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/migrations/0028_permission_meta.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0028_role_risk_fields.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0029_permission_metadata.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/migrations/0029_permission_metadata.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0030_userprofile_ui_prefs.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0031_enable_tenant_rls.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0032_organization_hierarchy.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0033_remove_organization_org_parent_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0034_check_constraints.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0035_organization_profile_fields.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0036_grc_organization_fields.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0037_remove_sso_mfa_fields.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0038_alter_organization_tax_id.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0039_tenant_api_calls_month_tenant_api_calls_today_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0040_tenant_admin_notification_message_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0041_rolev2_organization_parent_userprofile_primary_team_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0042_tenanthealthalertrule_tenanthealthmetric_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0043_broadcasttemplate_scheduledbroadcast_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0044_alter_scopedpermission_resource_roletemplate.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/migrations/0044_alter_scopedpermission_resource_roletemplate.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0045_rolev2_tags.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0046_remove_business_unit_and_update_team_types.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0047_sync_rbac_permissions.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/migrations/0047_sync_rbac_permissions.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0048_remove_userprofile_role.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0049_alter_scopedpermission_resource.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/migrations/0049_alter_scopedpermission_resource.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/0999_rename_rolev2_to_role.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1000_alter_role_options_alter_role_tenant.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1001_drop_legacy_rbac_tables.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/migrations/1001_drop_legacy_rbac_tables.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1002_recreate_role_permissions_through_table.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/migrations/1002_recreate_role_permissions_through_table.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1003_alter_scopedpermission_resource.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/migrations/1003_alter_scopedpermission_resource.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1004_alter_scopedpermission_action.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/migrations/1004_alter_scopedpermission_action.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1005_add_device_and_session_models.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1006_alter_scopedpermission_resource.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/migrations/1006_alter_scopedpermission_resource.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1007_add_dashboard_resource.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1008_entitlements_catalog.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1009_seed_owner_internal.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1010_subscription_split.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1011_alter_catalogsubscription_id_alter_creditgrant_id_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1012_merge_20251105_2056.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1013_delete_rolev2_remove_role_archived_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1014_notification_columns_and_locale_fields.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1015_merge_20251122_2008.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1016_add_account_models.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1017_assign_demo_admin.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1018_remove_demo_fullaccess_prod.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1019_add_integrations_permissions.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/migrations/1019_add_integrations_permissions.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1020_add_user_search_trgm_indexes.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1021_role_risk_level_role_risk_meta_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1022_userprofile_rls_by_user_id.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1023_standardize_rls_gucs.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1024_account_assetentity_fk.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1025_account_roles_permissions_auth.py",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "backend/accounts/migrations/1025_account_roles_permissions_auth.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/migrations/1025_account_roles_permissions_auth.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1026_remove_account_accounts_ac_tenant__auth_idx_and_more.py",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "backend/accounts/migrations/1026_remove_account_accounts_ac_tenant__auth_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1027_add_risk_rules_permissions.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/migrations/1027_add_risk_rules_permissions.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1028_add_account_risk_fields.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1029_add_finding_template_model.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1030_role_is_template_role_source_template_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1031_role_templates_global.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1032_remove_role_accounts_role_template_requires_null_tenant_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1033_drop_scopedpermission_and_legacy_role_fields.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/migrations/1033_drop_scopedpermission_and_legacy_role_fields.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1034_alter_userroleassignment_scope_type_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1035_roleriskpolicy.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1036_account_oauth_scopes.py",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "backend/accounts/migrations/1036_account_oauth_scopes.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1037_add_external_avatar_url.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1038_grant_demo_admin_v3.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1039_rbac_homogenization.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/migrations/1039_rbac_homogenization.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1040_remove_permissionauditlog_actor_and_more.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/migrations/1040_remove_permissionauditlog_actor_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1041_alter_role_permissions_v3.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/migrations/1041_alter_role_permissions_v3.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1042_migrate_permissions_to_canonical.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/migrations/1042_migrate_permissions_to_canonical.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1043_access_grants_and_scope_types.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1044_tenant_slug_global_unique.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1045_rename_accounts_acc_grantor_status_idx_accounts_ac_grantor_970445_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1046_tenant_onboarding_apps_score_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1047_tenant_dns_discovery_seed_fields.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1048_authanalyticssummary_authevent.py",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "backend/accounts/migrations/1048_authanalyticssummary_authevent.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1048_seed_free_plan.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1049_add_domain_role_exposure.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1050_change_domain_role_to_roles_array.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1051_tenant_profiles.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1052_seed_tenant_profiles.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1053_tenant_profile_templates.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1054_seed_tenant_profile_templates.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1055_role_templates_scope_and_profiles.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1056_alter_role_organization_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1057_tenantdomain_asset_entity.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1058_role_template_visibility_and_auto_create.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1059_fix_account_asset_fk_constraint.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1060_enforce_userprofile_rls.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1061_external_groups.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1062_rename_accounts_ex_tenant__3a632a_idx_accounts_ex_tenant__0c1f4d_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1063_role_is_platform_staff.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1064_platform_roles.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1065_usersession_realm_enforcement.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1066_remove_platformroleassignment_platform_role_assignment_user_role_uniq_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1067_consolidate_data_models.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1068_alter_organization_options_alter_team_options_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1069_documentslot_and_status.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1070_platform_role_assignment_starts_at.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1071_merge_20260202_1350.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1072_seed_default_platform_roles.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1073_feature_key_allow_dots.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1074_aeptus_support_access.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1075_alter_usertenantmembership_role.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1076_userprofile_rls_insert_policy.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1077_migrate_platform_roles_to_canonical_rbac.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/migrations/1077_migrate_platform_roles_to_canonical_rbac.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1078_userprofile_rls_include_memberships.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1079_userprofile_archived_at.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1080_profile_integrity_jobs.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1081_impersonation_ticket_and_request_id.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1082_alter_tenant_options.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1083_alter_scheduledbroadcast_status.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1084_tenant_profile_fk_and_framework_template.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1085_seed_baseline_framework_templates.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/1086_merge_20260305_1932.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/FOLDER.migrations.md",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/migrations/__init__.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/accounts/permissions_base.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/rbac.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/rbac_audit_models.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/rbac_canonical.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/rbac_helpers.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/rbac_models.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/rbac_permissions.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/rbac_scope.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/rbac_signals.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/tests/test_rbac_access_engine.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/tests/test_rbac_lifecycle_tick.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/tests/test_rbac_on_behalf_audit.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/accounts/tests/test_rbac_team_auto_assign.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/adn/migrations/0001_initial.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/adn/migrations/0002_enable_rls.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/adn/migrations/0003_fix_category_slug_uniqueness.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/adn/migrations/0004_pipelinerun_enrichmentqueue_directorysignal_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/adn/migrations/0005_localproviderentry_localserviceentry_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/adn/migrations/0006_remove_localproviderentry_unique_local_provider_domain_per_tenant_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/adn/migrations/0007_add_schema_version.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/adn/migrations/0008_directorycategory_expected_at_onboarding.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/adn/migrations/0009_add_app_metadata_facts.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/adn/migrations/0010_expand_fact_types.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/adn/migrations/0011_add_category_owner_fields.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/adn/migrations/0012_pipelinerun_add_adn_onboarding_enrich_stage.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/adn/migrations/0013_category_owner_delegation.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/adn/migrations/0014_pipelinestageconfig.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/adn/migrations/0015_remove_directoryfact_fact_single_target_entity_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/adn/migrations/0016_sitemap_supply_chain_choice_expansions.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/adn/migrations/0017_rename_enrichmentqueue_pipelinequeue.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/adn/migrations/0018_rename_adn_pipelin_target__70e8a1_idx_adn_pipelin_target__d13f85_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/adn/migrations/0019_pipelinebatch.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/adn/migrations/0020_rename_adn_pipelin_status_batch_idx_adn_pipelin_status_90c11e_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/adn/migrations/__init__.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/adn/permissions.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/adn/tests/test_permissions.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/ai_providers/migrations/0001_initial.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/ai_providers/migrations/0002_seed_providers.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/ai_providers/migrations/__init__.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/analytics/migrations/0001_initial.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/analytics/migrations/__init__.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/api_keys/migrations/0001_initial.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/api_keys/migrations/0002_rename_api_keys_tenant__a3f8b1_idx_api_keys_tenant__aa40c3_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/api_keys/migrations/0003_unique_constraints.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/api_keys/migrations/0004_apikey_user.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/api_keys/migrations/__init__.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/api_usage/migrations/0001_initial.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/api_usage/migrations/0002_enable_rls.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/api_usage/migrations/0003_rename_api_deprec_tenant_status_idx_api_depreca_tenant__60e9d0_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/api_usage/migrations/0004_brin_indexes.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/api_usage/migrations/0005_merge_20251001_1316.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/api_usage/migrations/0006_standardize_rls_gucs.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/api_usage/migrations/__init__.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/audit/migrations/0001_initial.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/audit/migrations/0002_rls_and_brin.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/audit/migrations/0003_dedup_unique.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/audit/migrations/0003_partition_shadow_table.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/audit/migrations/0004_alter_auditeventv2_options_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/audit/migrations/0004_audit_export_job.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/audit/migrations/0005_audit_ingest_keys.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/audit/migrations/0005_audit_phase3_enhancements.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/audit/migrations/0006_auditpolicy_legal_hold.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/audit/migrations/0007_auditpolicy_retention_status.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/audit/migrations/0008_auditeventv2_perf_indexes.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/audit/migrations/0009_merge_0004_0008.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/audit/migrations/0010_drop_audit_event_legacy.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/audit/migrations/0011_alter_auditexportjob_id.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/audit/migrations/0012_auditexportjob_expires_at_auditexportjob_format_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/audit/migrations/0013_standardize_rls_gucs.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/audit/migrations/0014_actor_id_string.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/audit/migrations/FOLDER.migrations.md",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/audit/migrations/__init__.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/automations/migrations/0001_initial.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/automations/migrations/0002_definition.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/automations/migrations/0003_data_model_rest.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/automations/migrations/0004_run_logs.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/automations/migrations/0005_enable_rls.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/automations/migrations/0006_check_constraints.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/automations/migrations/0007_performance_indexes.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/automations/migrations/0008_brin_indexes.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/automations/migrations/0009_partial_indexes.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/automations/migrations/0010_remove_automationdefinition_auto_def_tenant_status_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/automations/migrations/0011_merge_20251106_2056.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/automations/migrations/0012_remove_automationdefinition_created_by_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/automations/migrations/0013_standardize_rls_gucs.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/automations/migrations/0014_eventdeadletter_payload_json.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/automations/migrations/__init__.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/collaboration/migrations/0001_initial.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/collaboration/migrations/0002_review_models.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/collaboration/migrations/0003_rename_collaborati_locatio_idx_collaborati_locatio_8dcb41_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/collaboration/migrations/__init__.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/community/migrations/0001_initial.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/community/migrations/0002_enable_rls_policies.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/community/migrations/0003_alter_implicitsignal_target_type.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/community/migrations/__init__.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/controls/migrations/0001_initial.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/controls/migrations/0002_performance_indexes.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/controls/migrations/0003_custom_dashboards.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/controls/migrations/0004_remove_controldefinition_ctrl_tenant_status_domain_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/controls/migrations/0005_add_control_assessment_items.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/controls/migrations/0006_add_scope_dsl_fields.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/controls/migrations/0007_rename_ctrl_item_tenant_occ_idx_controls_co_tenant__9e7728_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/controls/migrations/0008_access_review_fields.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/controls/migrations/0009_item_validity_fields.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/controls/migrations/0009_rename_controls_co_tenant_c_kind_idx_controls_co_tenant__9e629c_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/controls/migrations/0010_merge_20251007_1220.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/controls/migrations/0010_occurrence_signoff_fields.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/controls/migrations/0011_merge_20251007_1252.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/controls/migrations/0012_rename_controls_occ_signoff_due_idx_controls_co_signoff_7bb034_idx.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/controls/migrations/0013_controldefinition_business_unit_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/controls/migrations/0014_add_composite_indexes.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/controls/migrations/0014_add_performance_indexes.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/controls/migrations/0015_merge_20251104_0914.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/controls/migrations/0016_evidence_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/controls/migrations/0017_add_search_vector_control.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/controls/migrations/0018_controldefinition_idx_control_search.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/controls/migrations/0019_controldefinition_idx_control_search.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/controls/migrations/0020_enable_rls.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/controls/migrations/0021_evidence_artifact_scan_fields.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/controls/migrations/0022_framework_requirement_and_policy_mapping.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/controls/migrations/0023_populate_framework_requirements.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/controls/migrations/0024_rename_controls_fr_framewo_idx_cat_controls_fr_framewo_83042f_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/controls/migrations/__init__.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/controls/permissions.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/controls/tests/test_permissions.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/controls/tests/test_rbac_boundary.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/core/management/commands/create_search_permissions.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/core/migrations/0001_initial.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/core/migrations/0003_inapp_security_evidence.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/core/migrations/0004_alerts_evidence_meta_url.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/core/migrations/0005_auditevent_healthcheck_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/core/migrations/0006_outbound_email_job.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/core/migrations/0007_emailjob_partial_idx.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/core/migrations/0008_outbound_email_bodyhash_unique.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/core/migrations/0009_remove_outboundemailjob_core_emailjob_triplet_uniq_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/core/migrations/0010_pg_stat_statements_extension.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/core/migrations/0011_delete_auditevent.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/core/migrations/0012_drop_core_auditevent.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/core/migrations/0013_rlsauditevent.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/core/migrations/0014_designsystempage_designsystemcomponent_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/core/migrations/0015_add_planned_components.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/core/migrations/0016_add_resource_permission_models.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/core/migrations/0016_add_resource_permission_models.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/core/migrations/0017_rlsauditevent.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/core/migrations/0018_change_default_visibility_to_tenant.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/core/migrations/0019_tenantattribute_moduleattributeconfig_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/core/migrations/0020_queryperformancelog_queryperformancestats.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/core/migrations/0021_rename_core_queryp_created_af2bd6_idx_core_queryp_created_ff0917_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/core/migrations/0022_merge_20251106_2056.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/core/migrations/0023_search_analytics_models.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/core/migrations/0024_alter_searchanalytics_created_by_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/core/migrations/0025_export_job.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/core/migrations/0026_enable_rls_core_export_job.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/core/migrations/0027_alter_exportjob_format_alter_exportjob_status.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/core/migrations/FOLDER.migrations.md",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/core/migrations/__init__.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/core/permissions.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/core/permissions/__init__.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/core/permissions/decorators.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/core/permissions/helpers.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/core/permissions/policy.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/core/permissions/rbac.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/core/permissions/rls_queryset_manager.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/core/permissions/test_utils.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/core/permissions/tests/__init__.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/core/permissions/tests/test_decorators.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/core/permissions/tests/test_helpers.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/core/permissions/tests/test_policy.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/core/tests/test_search/test_search_permissions.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/deployment-guide.md",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "backend/directory/migrations/0001_initial.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/directory/migrations/0002_bitemporal_constraints.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/directory/migrations/0003_add_service_offering_technology.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/directory/migrations/0004_bitemporal_exclusion_constraints.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/directory/migrations/0005_check_constraints.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/directory/migrations/0006_alter_technologycomponent_unique_together_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/directory/migrations/0007_service_offering_technology_constraints_and_cleanup.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/directory/migrations/0008_legalentity_categories_serviceoffering_categories_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/directory/migrations/0009_legalentity_serviceoffering_expansion.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/directory/migrations/0010_legalentity_industry_sanctions_jurisdiction.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/directory/migrations/0011_allow_null_legal_entity_on_service_offering.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/directory/migrations/0012_remove_technologycomponent_categories_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/directory/migrations/0013_fix_techcat_null_distinct.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/directory/migrations/0014_remove_serviceoffering_tags.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/directory/migrations/0015_technologyproduct_categories.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/directory/migrations/0016_technologycategory_is_active.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/directory/migrations/__init__.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/documents/deployment-guide.md",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "backend/documents/migrations/0001_initial.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/documents/migrations/0002_rename_doc_t_type_deleted_idx_documents_d_tenant__5ef7dd_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/documents/migrations/0003_enable_rls.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/documents/migrations/0004_expand_doctype_and_relations.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/documents/migrations/0005_documentslot_and_status.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/documents/migrations/0006_documenttypeprofile.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/documents/migrations/__init__.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0001_initial.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0002_add_owned_resource_mixin.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0002_initial.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0002_riskrule_riskrulefielddefinition_riskruleexecution.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0003_add_business_security_ownership.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0004_alter_asset_visibility_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0005_remove_asset_criticality_asset_service_asset_tier.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0006_add_composite_indexes.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0006_add_performance_indexes.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0007_merge_20251104_0914.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0008_remove_asset_env_asset_lifecycle_risk_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0009_merge_20251106_2056.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0010_remove_asset_environment_owner_t_925258_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0011_asset_idx_asset_type_tier_stat_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0012_add_search_vector_asset.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0013_asset_idx_asset_search.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0014_asset_managed_by_thirdpartyentity.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0015_alter_asset_unique_together_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0017_bitemporal_table_maintenance_tuning.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0018_asset_constraints_and_assettechnology_pair_unique.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0019_enable_rls.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0020_standardize_risk_fields.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0021_merge_20251216_1225.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0022_rename_env_riskrule_tenant_target_idx_environment_tenant__8ca752_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0023_riskrulefielddefinition_category.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0024_add_asset_risk_breakdown.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0025_sprint14_asset_enhancements.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0026_rename_env_compmap_t_stat_idx_environment_tenant__24f617_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0026_update_asset_search_vector_business_unit_option.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0027_merge_20251224_0905.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0028_asset_hosting_model_asset_local_service_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0029_asset_data_model_v12_1.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0030_remove_asset_idx_asset_category_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0031_risk_rule_library.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0032_remove_orgrulevisibility_unique_org_rule_visibility_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0033_add_asset_domain_registration_fields.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0034_alter_asset_asset_type.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0035_domain_analyzer_integration_sprint16.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0036_asset_discovery_tracking_fields.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0036_rename_idx_certhistory_asset_environment_tenant__3daa83_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0037_merge_20260109_0700.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0038_threat_intelligence_traffic_ranking.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0039_remove_asset_idx_asset_threat_malicious_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0040_technology_fingerprinting.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0041_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0042_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0043_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0044_asset_discovery_sources.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0045_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0046_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0047_update_threatintelligencecheck_constraint.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0048_backfill_asset_discovery_sources.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0049_asset_directory_category.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0050_remove_technologycategory_parent_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/0051_remove_asset_business_function_id_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/migrations/__init__.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/environment/permissions.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/environment/risk_rule_library_permissions.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/environment/risk_rule_permissions.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/events/migrations/0001_initial.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/events/migrations/0002_add_owned_resource_mixin.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/events/migrations/0003_add_business_security_ownership.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/events/migrations/0004_alter_event_visibility_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/events/migrations/0005_incident.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/events/migrations/0006_delete_incident.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/events/migrations/0007_incident_assetentity.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/events/migrations/0008_alter_incident_created_at_alter_incident_created_by_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/events/migrations/0009_alter_incident_business_owner_team_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/events/migrations/__init__.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/frameworks/migrations/__init__.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/gcp/deploy.sh",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "backend/gcp/setup-search-infrastructure.sh",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "backend/guide-migrations.md",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/information/migrations/0001_initial.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/information/migrations/0001_initial_ims_models.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/information/migrations/0002_alter_document_content_type_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/information/migrations/0003_merge_20251106_2056.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/information/migrations/0004_assetentity_fks_and_contenttypes.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/information/migrations/0005_alter_privacyprofile_content_type.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/information/migrations/__init__.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/information/models/migration.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/information/serializers/migration.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/integrations/migrations/0001_initial.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/integrations/migrations/0002_alter_integrationconnection_provider.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/integrations/migrations/0003_slackinstall.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/integrations/migrations/0004_rename_integrations_slack_tenant_team_idx_integration_tenant__f6ace6_idx.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/integrations/migrations/0005_enhance_integrationconnection_for_adapter_pattern.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/integrations/migrations/0006_enhance_integration_connection.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/integrations/migrations/0007_integrationfieldmapping.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/integrations/migrations/0008_scaling_architecture.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/integrations/migrations/0009_alter_integrationprovider_options_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/integrations/migrations/0010_integrationaction_integrationdatapoint.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/integrations/migrations/0011_seed_google_workspace_actions_complete.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/integrations/migrations/0012_integrationaction_category.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/integrations/migrations/0013_seed_google_workspace_webhooks.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/integrations/migrations/0014_seed_slack_provider_and_actions.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/integrations/migrations/0015_seed_github_provider_and_actions.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/integrations/migrations/0016_add_is_automation_enabled.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/integrations/migrations/0017_integrationaction_integration_auto_en_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/integrations/migrations/0018_integration_sync_history.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/integrations/migrations/0019_add_sync_history_data_snapshots.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/integrations/migrations/0020_add_sync_type_choices.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/integrations/migrations/0021_rename_nango_connection_id.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/integrations/migrations/0022_normalize_integration_provider_categories.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/integrations/migrations/0023_seed_microsoft_365_provider.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/integrations/migrations/0024_seed_microsoft_teams_provider.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/integrations/migrations/0025_normalize_connected_status_to_active.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/integrations/migrations/0026_seed_google_workspace_provider.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/integrations/migrations/__init__.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/integrations/tests/test_token_lifecycle_guardrails.py",
        "area": "secrets",
        "level": "high",
        "reason": "sensitive credential surface"
      },
      {
        "scope": "backend/integrations/tests/test_token_refresh.py",
        "area": "secrets",
        "level": "high",
        "reason": "sensitive credential surface"
      },
      {
        "scope": "backend/integrations/token-lifecycle-standard.md",
        "area": "secrets",
        "level": "high",
        "reason": "sensitive credential surface"
      },
      {
        "scope": "backend/knowledge/migrations/0001_initial.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/knowledge/migrations/0002_remove_controlmapping_unique_policy_requirement_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/knowledge/migrations/__init__.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/localization/migrations/0001_initial.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/localization/migrations/0002_add_owned_resource_mixin.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/localization/migrations/0003_add_business_security_ownership.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/localization/migrations/0004_alter_glossaryterm_visibility_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/localization/migrations/0005_add_analytics_models.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/localization/migrations/0006_alter_translationchangelog_created_by_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/localization/migrations/0007_translation_ai_config.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/localization/migrations/__init__.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/manual-deploy-with-verify.sh",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "backend/mapping_intelligence/migrations/0001_initial.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/mapping_intelligence/migrations/0002_add_missing_fields.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/mapping_intelligence/migrations/0002_fielddefinition_mapping_int_synonym_49b140_gin.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/mapping_intelligence/migrations/0003_add_aimachinesettings.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/mapping_intelligence/migrations/0003_fielddefinition_tenant_scope.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/mapping_intelligence/migrations/0003_rename_mapping_int_entity__idx_mapping_int_entity__9ab5a0_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/mapping_intelligence/migrations/0004_merge_20251020_1449.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/mapping_intelligence/migrations/0005_add_versioning_and_constraints.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/mapping_intelligence/migrations/0007_add_performance_indexes.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/mapping_intelligence/migrations/0008_merge_20251106_2056.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/mapping_intelligence/migrations/0009_mappinghistory_updated_at_mappinghistory_updated_by_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/mapping_intelligence/migrations/0010_merge_20260105_1105.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/mapping_intelligence/migrations/0011_remove_fielddefinition_mapping_int_entity__030d87_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/mapping_intelligence/migrations/__init__.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/mapping_intelligence/permissions.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/menu_overrides/migrations/0001_initial.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/menu_overrides/migrations/0002_add_navigation_analytics.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/menu_overrides/migrations/__init__.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/middleware/rbac_enforcement.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/onboarding/migrations/0001_initial.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/onboarding/migrations/0002_onboardingruntimestate.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/onboarding/migrations/__init__.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/operational/migrations/0001_initial.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/operational/migrations/0002_event_sourcing_triggers.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/operational/migrations/0003_fix_event_sourcing_trigger.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/operational/migrations/0004_trigger_request_id.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/operational/migrations/0005_trigger_request_id_metadata.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/operational/migrations/0006_trigger_update_merge_guard.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/operational/migrations/0007_trigger_merge_guard_jsonb.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/operational/migrations/__init__.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/ops/scripts/deploy-celery-jobs.sh",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "backend/ops/scripts/deploy-rbac-seed.sh",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/ops/scripts/deploy-rbac-seed.sh",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "backend/ops/scripts/execute-rbac-seed.sh",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/ops/scripts/run-migrations.sh",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/ops/scripts/seed-rbac-permissions.sh",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/page_actions/migrations/0001_initial.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/page_actions/migrations/0002_remove_customaction_unique_custom_action_per_org_page_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/page_actions/migrations/0003_standardize_rls_gucs.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/page_actions/migrations/__init__.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/page_actions/permissions.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/page_actions/services/permission_service.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/page_actions/tests/test_permission_service.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/posture/finding_template_library_permissions.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/posture/migrations/0001_initial.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/posture/migrations/0002_add_owned_resource_mixin.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/posture/migrations/0003_add_business_security_ownership.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/posture/migrations/0004_alter_campaign_visibility_alter_finding_visibility_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/posture/migrations/0005_add_search_vector_finding.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/posture/migrations/0006_finding_idx_finding_search.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/posture/migrations/0007_alter_campaign_scope_assets_alter_finding_asset_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/posture/migrations/0008_add_finding_likelihood_and_targets.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/posture/migrations/0009_add_finding_template_model.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/posture/migrations/0010_finding_template_library.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/posture/migrations/0011_seed_finding_template_library.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/posture/migrations/0012_rename_posture_ftl_category_status_idx_posture_ftl_cat_status_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/posture/migrations/__init__.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/run_migrations.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/scripts/audit_rbac_migration.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/scripts/audit_rbac_migration.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/scripts/debug/test_automations_permission_debug.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/scripts/debug/test_rbac_migration.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/scripts/debug/test_rbac_migration.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/setup-auto-deploy.sh",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "backend/tasks/migrations/0001_initial.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/tasks/migrations/0002_tasklink.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/tasks/migrations/0003_tasksavedview.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/tasks/migrations/0004_task_tags.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/tasks/migrations/0005_task_comments_watchers.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/tasks/migrations/0006_task_attachment.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/tasks/migrations/0007_checklist_item.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/tasks/migrations/0008_alter_checklistitem_created_at_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/tasks/migrations/0008_task_workflow_sla.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/tasks/migrations/0009_task_provenance.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/tasks/migrations/0010_merge_20251006_1220.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/tasks/migrations/0011_add_owned_resource_mixin.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/tasks/migrations/0016_task_completion_rule.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/tasks/migrations/0017_add_business_security_ownership.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/tasks/migrations/0018_alter_checklistitem_visibility_alter_task_visibility_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/tasks/migrations/0019_add_search_vector_task.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/tasks/migrations/0020_task_idx_task_search.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/tasks/migrations/0021_enable_rls.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/tasks/migrations/0022_add_task_decisions.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/tasks/migrations/0023_convert_task_decision_to_task_type.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/tasks/migrations/0024_enforce_single_pending_task_decision.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/tasks/migrations/0025_alter_task_status_alter_task_type.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/tasks/migrations/__init__.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/tests/admin/test_admin_notifications_policy_rbac.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/tests/admin/test_admin_permission_audit.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/tests/admin/test_admin_permission_audit_correlation.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/tests/admin/test_admin_roles_rbac_edit_allow.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/tests/admin/test_admin_roles_rbac_edit_deny.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/tests/admin/test_admin_users_rbac_allow.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/tests/audit/test_audit_events_rbac_deny_audit.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/tests/audit/test_audit_export_rbac_deny.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/tests/audit/test_audit_export_rbac_superuser.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/tests/audit/test_audit_rbac.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/tests/audit/test_audit_rbac_endpoints.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/tests/audit/test_audit_registry_billing_vendor_required.py",
        "area": "billing",
        "level": "high",
        "reason": "billing logic"
      },
      {
        "scope": "backend/tests/audit/test_audit_registry_permissions_required.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/tests/critical/test_auth_oidc.py",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "backend/tests/integration/test_auth0_idp_asset_bootstrap.py",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "backend/tests/integration/test_collaboration_authorization.py",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "backend/tests/integration/test_db_viewer_rbac_allow.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/tests/integration/test_schema_permissions.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/tests/integration/test_schema_ui_permissions.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/tests/integration/test_secret_hashing.py",
        "area": "secrets",
        "level": "high",
        "reason": "sensitive credential surface"
      },
      {
        "scope": "backend/tests/integration/test_teams_rbac.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/tests/integration/test_teams_rbac_allow.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/tests/integration/test_thirdparty_authorization.py",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "backend/tests/integration/test_thirdparty_relationship_authorization.py",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "backend/tests/security/test_auth_flows_comprehensive.py",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "backend/tests/security/test_auth_login_ratelimit.py",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "backend/tests/security/test_auth_logout_csrf.py",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "backend/tests/security/test_auth_session.py",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "backend/tests/security/test_impersonation_rbac.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/tests/security/test_rbac_admin_api.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/tests/security/test_rbac_casl_mapping.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/tests/security/test_rbac_forbidden_json.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/tests/security/test_rbac_forbidden_json_shape.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/tests/security/test_rbac_risk_matrix.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/tests/security/test_rbac_risk_recalc_command.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/tests/security/test_rbac_settings_guard.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/tests/security/test_thirdparty_unauth_endpoints.py",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "backend/tests/suppliers/test_suppliers_reports_rbac.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "backend/thirdparties/migrations/0001_initial.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0002_enable_rls_policies.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0003_bitemporal_constraints.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0004_rename_tables_suppliers_to_thirdparties.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0005_rename_thirdparties_a_tenant__c63003_idx_thirdpartie_tenant__0c77fd_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0006_rename_thirdparties_a_tenant__c63003_idx_thirdpartie_tenant__0c77fd_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0007_add_asset_service_offering.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0008_rename_thirdparties_a_tenant__c63003_idx_thirdpartie_tenant__0c77fd_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0009_supplier_graph_view.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0010_add_missing_fields.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0011_enable_rls.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0012_remove_thirdpartyrelationship_thirdparty_rel_unique_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0012_suppliers_saved_view.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0013_asset_service_offering_constraints.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0014_check_constraints.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0015_performance_indexes.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0016_partial_indexes.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0017_add_frontend_aligned_fields.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0018_add_gdpr_data_privacy_models.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0019_remove_dataprivacycontact_created_by_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0020_add_privacy_enhancements.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0021_remove_dataprivacycontact_created_by_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0022_repair_privacy_columns.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0023_remove_asset_asset_tenant_type_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0024_remove_asset_asset_tenant_type_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0025_alter_document_content_type_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0026_supplierassessment_supplierchangerequest_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0027_dataprivacycontact_dataprivacyprofile_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0028_merge_20251023_1923.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0029_tprm_policy_owner_team_doc_source.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0030_rename_tp_tenant_owner_user_idx_thirdpartie_tenant__4ef8e4_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0031_add_business_security_ownership.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0032_dataprivacycontact_dataprivacyprofile_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0033_alter_thirdparty_visibility.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0034_alter_thirdparty_relationship_types.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0035_thirdparty_frameworks_alter_thirdparty_tags.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0036_alter_thirdparty_tags.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0037_add_composite_indexes.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0037_add_performance_indexes.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0038_merge_20251104_0914.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0039_remove_directorylinkconfig_tp_link_sync_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0041_search_extensions_and_indexes.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0042_add_search_vector_thirdparty.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0043_thirdparty_idx_thirdparty_search.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0044_thirdparty_entity_versioning.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0045_dataprivacyprofile_third_party_entity.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0046_rename_thirdparty_tenant_entity_idx_thirdpartie_tenant__205199_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0047_standardize_rls_gucs.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0048_alter_directorylinkconfig_linked_legal_entity_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0049_fix_thirdparty_no_overlap_valid_to_infinity.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0050_bitemporal_table_maintenance_tuning.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0051_standardize_risk_fields.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0052_alter_thirdparty_risk_factors_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0053_directorylinkconfig_linked_local_provider.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0054_alter_thirdparty_lifecycle_status.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0055_thirdparty_adn_parity_fields.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0056_functionalrole_industrycodecrosswalk_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0057_seed_functional_roles.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0058_seed_industry_crosswalk.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0059_thirdparty_adn_parity_fields.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0060_supplier_directory_category.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0061_thirdparty_control_frameworks.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0062_migrate_frameworks_m2m.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/0063_alter_thirdparty_frameworks.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/FOLDER.migrations.md",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/thirdparties/migrations/__init__.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/webhooks/migrations/0001_initial.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/webhooks/migrations/0002_rename_webhooks_de_subscri_f5d8c1_idx_webhooks_de_subscri_f97236_idx_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/webhooks/migrations/0003_unique_constraints.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/webhooks/migrations/0004_add_owned_resource_mixin.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/webhooks/migrations/0005_add_business_security_ownership.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/webhooks/migrations/0006_alter_webhookdelivery_visibility_and_more.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/webhooks/migrations/0007_hash_webhook_secrets.py",
        "area": "secrets",
        "level": "high",
        "reason": "sensitive credential surface"
      },
      {
        "scope": "backend/webhooks/migrations/0007_hash_webhook_secrets.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/webhooks/migrations/0008_alter_webhooksubscription_secret.py",
        "area": "secrets",
        "level": "high",
        "reason": "sensitive credential surface"
      },
      {
        "scope": "backend/webhooks/migrations/0008_alter_webhooksubscription_secret.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "backend/webhooks/migrations/__init__.py",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "contracts/migration-to-schemathesis.md",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "devops/grafana/dashboards/rbac-dashboard.json",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "devops/prometheus/rules/rbac-alerts.yml",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "docs/access-auth.md",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "docs/agents/context/admin.billing.md",
        "area": "billing",
        "level": "high",
        "reason": "billing logic"
      },
      {
        "scope": "docs/api/rbac-api-reference.md",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "docs/api/rbac-api.md",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "docs/api/rbac-openapi.json",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "docs/api/rbac-quick-reference.md",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "docs/architecture/architecture-deployment.md",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "docs/architecture/rbac-architecture.md",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "docs/architecture/security-audit-rbac.md",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "docs/design-system/automated-deployment-setup.md",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "docs/design-system/design-tokens-tier-guide.md",
        "area": "secrets",
        "level": "high",
        "reason": "sensitive credential surface"
      },
      {
        "scope": "docs/feature-flags/catalog-key-migration.md",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "docs/feature-specs/admin/page-actions/09-deployment-guide.md",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "docs/feature-specs/controls/deployment-checklist.md",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "docs/feature-specs/information/my-environment-rbac-integration.md",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "docs/feature-specs/rbac/admin-roles-review-and-cleanup.md",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "docs/feature-specs/rbac/rbac-spec.md",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "docs/feature-specs/search-deployment-guide.md",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "docs/guides/authentication-setup.md",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "docs/guides/cost-optimized-deployment.md",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "docs/guides/deployment-guide-permissions.md",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "docs/guides/deployment-guide-permissions.md",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "docs/guides/multi-tenant-deployment-critical.md",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "docs/guides/post-deployment-setup.md",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "docs/guides/post-deployment-verification.md",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "docs/guides/rbac-admin-guide.md",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "docs/permissions.md",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "docs/plans/infrastructure-options-comparison.md",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "docs/prd/rbac-simplified-design.md",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "docs/rbac-cache-implementation.md",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "docs/reference/rbac.yaml",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "docs/reference/reference-rbac-permission-sync.md",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "docs/runbooks/deploy-admin.md",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "docs/runbooks/deployment-checklist.md",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "docs/runbooks/rbac-operations-runbook.md",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "docs/runbooks/rbac-risk-policy.md",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "docs/runbooks/runbook-deployment-best-practices.md",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "docs/runbooks/runbook-production-deployment.md",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "docs/secret-management-plan.md",
        "area": "secrets",
        "level": "high",
        "reason": "sensitive credential surface"
      },
      {
        "scope": "docs/security/rbac-risk-policy.md",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "e2e/fixtures/auth.fixture.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "e2e/page-objects/auth/login.page.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "e2e/tests/auth/authentication.spec.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "manual-deployment-steps.md",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "migration-complete.md",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "migration-status.md",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "migrations-applied-success.md",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "packages/app-shared/src/app/AuthenticatedApp.tsx",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/auth/AbilityContext.shared.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/auth/AbilityProvider.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/auth/AbilityProviderRoot.tsx",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/auth/AuthError.tsx",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/auth/FOLDER.auth.md",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/auth/NoTenantAccess.tsx",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/auth/SessionExpiryWarningProvider.tsx",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/auth/SessionGate.tsx",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/auth/UseEnvironmentPermissions.tsx",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/auth/UseEnvironmentPermissions.tsx",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/auth/__tests__/FOLDER.__tests__.md",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/auth/__tests__/permissionGrouping.test.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/auth/__tests__/permissionGrouping.test.ts",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/auth/ability.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/auth/can.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/auth/logoutBroadcast.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/auth/logoutClient.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/auth/permissionGrouping.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/auth/permissionGrouping.ts",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/auth/rbac-canonical.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/auth/rbac-canonical.ts",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/auth/session.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/auth/sessionExpiryWarningContext.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/auth/useSessionHeartbeat.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/components/admin/AdminBillingView.tsx",
        "area": "billing",
        "level": "high",
        "reason": "billing logic"
      },
      {
        "scope": "packages/app-shared/src/components/admin/OrgBillingOverviewView.tsx",
        "area": "billing",
        "level": "high",
        "reason": "billing logic"
      },
      {
        "scope": "packages/app-shared/src/components/admin/TenantBillingTab.tsx",
        "area": "billing",
        "level": "high",
        "reason": "billing logic"
      },
      {
        "scope": "packages/app-shared/src/components/admin/roles/BatchPermissionUpdates.tsx",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/components/admin/roles/PermissionConflictDetector.tsx",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/components/admin/roles/PermissionMatrix.tsx",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/components/admin/roles/PermissionMatrixSkeleton.tsx",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/components/admin/roles/PermissionsList.tsx",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/components/admin/roles/__tests__/BatchPermissionUpdates.test.tsx",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/components/auth/PermissionDenied.tsx",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/components/auth/PermissionDenied.tsx",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/constants/rbac-module-settings.md",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/constants/rbac.ts",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/features/admin/components/AdminBillingView.tsx",
        "area": "billing",
        "level": "high",
        "reason": "billing logic"
      },
      {
        "scope": "packages/app-shared/src/features/admin/components/roles/PermissionConflictDetector.tsx",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/features/admin/components/roles/PermissionMatrix.tsx",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/features/admin/components/roles/PermissionMatrixSkeleton.tsx",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/features/admin/components/roles/permissionConflictRules.ts",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/features/admin/components/roles/permissionMatrix.shared.ts",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/features/admin/hooks/useUsersAndPermissions.ts",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/features/admin/pages/AdminBillingPage.tsx",
        "area": "billing",
        "level": "high",
        "reason": "billing logic"
      },
      {
        "scope": "packages/app-shared/src/features/admin/pages/AdminMigrationDashboardPage.tsx",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "packages/app-shared/src/features/admin/pages/AdminMigrationDashboardPageView.tsx",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "packages/app-shared/src/features/auth/components/PermissionDenied.tsx",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/features/auth/components/PermissionDenied.tsx",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/features/auth/components/index.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/features/auth/index.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/features/auth/utils/ability.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/features/auth/utils/can.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/features/auth/utils/index.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/features/auth/utils/permissionGrouping.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/features/auth/utils/permissionGrouping.ts",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/features/auth/utils/session.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/features/information/hooks/useMigration.ts",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "packages/app-shared/src/features/information/pages/MigrationConflictsPage.tsx",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "packages/app-shared/src/features/information/pages/MigrationDashboardPage.tsx",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "packages/app-shared/src/features/information/pages/MigrationImportPage.tsx",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "packages/app-shared/src/features/org/pages/OrgBillingPage.tsx",
        "area": "billing",
        "level": "high",
        "reason": "billing logic"
      },
      {
        "scope": "packages/app-shared/src/hooks/admin/usePermissionsCatalog.ts",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/hooks/admin/useUsersAndPermissions.ts",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/hooks/information/useMigration.ts",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "packages/app-shared/src/hooks/lib/parsePermissions.ts",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/hooks/permissions/__tests__/useCanAccess.test.tsx",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/hooks/permissions/__tests__/usePermission.test.tsx",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/hooks/permissions/index.ts",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/hooks/permissions/testUtils.tsx",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/hooks/permissions/useAbility.ts",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/hooks/permissions/useCanAccess.ts",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/hooks/permissions/usePermission.ts",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/hooks/useOrgBillingApi.ts",
        "area": "billing",
        "level": "high",
        "reason": "billing logic"
      },
      {
        "scope": "packages/app-shared/src/lib/__tests__/permissions.test.ts",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/lib/permissions.ts",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/lib/personalTokensApi.ts",
        "area": "secrets",
        "level": "high",
        "reason": "sensitive credential surface"
      },
      {
        "scope": "packages/app-shared/src/pages/AuthLogoutPage.tsx",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/pages/platform/AuthAnalyticsPage.impl.tsx",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/pages/platform/AuthAnalyticsPage.tsx",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/preauth/debug.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/preauth/index.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/preauth/network.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/preauth/session.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/preauth/telemetry.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/preauth/theme.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/preauth/types.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/preauth/ui.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/preauth/utils.test.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/preauth/utils.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/router/Unauthorized.tsx",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/tests/admin.billing.a11y.test.tsx",
        "area": "billing",
        "level": "high",
        "reason": "billing logic"
      },
      {
        "scope": "packages/app-shared/src/tests/admin.billing.exportmenu.test.tsx",
        "area": "billing",
        "level": "high",
        "reason": "billing logic"
      },
      {
        "scope": "packages/app-shared/src/tests/admin.billing.mobile.test.tsx",
        "area": "billing",
        "level": "high",
        "reason": "billing logic"
      },
      {
        "scope": "packages/app-shared/src/tests/admin.billing.toolbar.smoke.test.tsx",
        "area": "billing",
        "level": "high",
        "reason": "billing logic"
      },
      {
        "scope": "packages/app-shared/src/tests/admin.users.rbac.banner.test.tsx",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/tests/api.credentials.test.ts",
        "area": "secrets",
        "level": "high",
        "reason": "sensitive credential surface"
      },
      {
        "scope": "packages/app-shared/src/tests/auth.can.test.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/tests/permission.gate.test.tsx",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/tests/router.unauthorized.ui.test.tsx",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/app-shared/src/tests/suppliers.directory.views.rbac.test.tsx",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/app-shared/src/types/rbac.ts",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/auth/package.json",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/auth/src/__tests__/permissionGrouping.test.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/auth/src/__tests__/permissionGrouping.test.ts",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/auth/src/__tests__/rbac-canonical.test.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/auth/src/__tests__/rbac-canonical.test.ts",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/auth/src/ability.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/auth/src/can.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/auth/src/index.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/auth/src/logout/broadcast.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/auth/src/logout/client.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/auth/src/logout/index.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/auth/src/permissionGrouping.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/auth/src/permissionGrouping.ts",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/auth/src/rbac-canonical.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/auth/src/rbac-canonical.ts",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/auth/src/session.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/auth/test-results/junit.xml",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/auth/tsconfig.json",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/auth/tsconfig.tsbuildinfo",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "packages/documentation/migration/page-checklist.json",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "packages/types/src/rbac.ts",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "packages/ui/.ai/design-tokens.json",
        "area": "secrets",
        "level": "high",
        "reason": "sensitive credential surface"
      },
      {
        "scope": "packages/ui/.ai/migration-rules.json",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "packages/ui/src/components/molecules/TokenPicker/TokenPicker.tsx",
        "area": "secrets",
        "level": "high",
        "reason": "sensitive credential surface"
      },
      {
        "scope": "packages/ui/src/components/molecules/TokenPicker/index.ts",
        "area": "secrets",
        "level": "high",
        "reason": "sensitive credential surface"
      },
      {
        "scope": "packages/ui/src/tokens/components.css",
        "area": "secrets",
        "level": "high",
        "reason": "sensitive credential surface"
      },
      {
        "scope": "packages/ui/src/tokens/index.css",
        "area": "secrets",
        "level": "high",
        "reason": "sensitive credential surface"
      },
      {
        "scope": "packages/ui/src/tokens/index.ts",
        "area": "secrets",
        "level": "high",
        "reason": "sensitive credential surface"
      },
      {
        "scope": "packages/ui/src/tokens/primitives.css",
        "area": "secrets",
        "level": "high",
        "reason": "sensitive credential surface"
      },
      {
        "scope": "packages/ui/src/tokens/semantic.css",
        "area": "secrets",
        "level": "high",
        "reason": "sensitive credential surface"
      },
      {
        "scope": "packages/ui/src/tokens/themes/dark.css",
        "area": "secrets",
        "level": "high",
        "reason": "sensitive credential surface"
      },
      {
        "scope": "postgres-18-migration-guide.md",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "rbac-cache-delivery.md",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "rbac-cache-quickstart.md",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "scripts/checks/check-customer-preauth-no-design-system.mjs",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "scripts/ci/check-endpoint-permissions.mjs",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "scripts/ci/check-permission-metadata.mjs",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "scripts/ci/check-route-permissions.sh",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "scripts/ci/check_migrations.sh",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "scripts/ci/validate-rbac-sync.mjs",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "scripts/deploy-types.cjs",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "scripts/deployment/build-production.sh",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "scripts/design-system/generate-token-json.mjs",
        "area": "secrets",
        "level": "high",
        "reason": "sensitive credential surface"
      },
      {
        "scope": "scripts/migration/audit-page-components.mjs",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "scripts/validate-deployment.sh",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "scripts/validation/validate_permissions.py",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      },
      {
        "scope": "scripts/verify-phase0-deployment.sh",
        "area": "infra",
        "level": "high",
        "reason": "infrastructure surface"
      },
      {
        "scope": "scripts/verify_migration.sh",
        "area": "migrations",
        "level": "high",
        "reason": "schema change area"
      },
      {
        "scope": "tests/contract/consumers/auth.contract.test.ts",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      },
      {
        "scope": "tools/mcp-mordor/src/tools/rbac.ts",
        "area": "permissions",
        "level": "high",
        "reason": "permission boundary"
      }
    ],
    "navigation_order": [
      ".claude/commands"
    ],
    "budget": {
      "max_anchors": 3,
      "max_files": 5,
      "max_snippets": 8,
      "dependency_depth": 1,
      "impact_depth": 1
    },
    "confidence": {
      "anchor_confidence": 0.75,
      "scope_confidence": 0.7
    }
  }
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
