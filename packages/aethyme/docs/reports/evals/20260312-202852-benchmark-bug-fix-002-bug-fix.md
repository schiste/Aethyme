# Eval Report: Fix failing test: manage permission does not imply share in ability-implications.test.ts

Last Updated: 2026-03-12

- Repository: `/tmp/benchmark-bug-fix-002`
- Generated: `2026-03-12T20:28:52.719736+00:00`
- Conditions: `control-cto-off, control-cto-on, explore, leverage`

## Summary

- Control (CTO off) prompt chars: `964`
- Control (CTO on) prompt chars: `963`
- Explore prompt chars: `956`
- Leverage prompt chars: `1079`
- Navigation items: `0`
- Risk items: `0`

### Scorecard

| Condition | Score | Tokens | Tool Calls |
| --- | --- | --- | --- |
| Control (CTO off) | 97.0 | 56K | - |
| Control (CTO on) | 94.0 | 66K | - |
| Explore | 94.0 | 98K | - |
| Leverage | 94.0 | 78K | - |

## Control (CTO off)

### Prompt

```text
A test is failing in this repository.

Repository path: /private/tmp/benchmark-bug-fix-002/control-cto-off
Test file: packages/auth/src/__tests__/ability-implications.test.ts
Run command: npx vitest run packages/auth/src/__tests__/ability-implications.test.ts

Failure output:
FAIL packages/auth/src/__tests__/ability-implications.test.ts
  x permission implications > manage:suppliers grants share permission via ability builder
    -> expected true to be false
  x permission implications > manage:suppliers grants all expected permissions
    -> share check failed
  x permission implications > getImpliedActions for manage includes share
    -> expected array to contain 'share'
  x permission implications > actionImplies correctly checks manage -> share
    -> expected false to be true

The test file is correct. The bug is in the source code.
Find the root cause and fix the bug so all tests pass.
You can verify your fix by running the test command above.
```

### Run Metrics

- command: `codex exec -s danger-full-access --output-schema /tmp/aethyme-eval-output-schema.json -o /tmp/aethyme-eval-control-cto-off-result.json - < /tmp/aethyme-eval-control-cto-off-prompt.txt`
- exit code: `0`
- input tokens: `56947`
- output tokens: `0`
- retries: `0`
- review burden: `0`
- wall time: `167.000s`

### Final Output Message

```text
Restored `Action.SHARE` to the canonical `manage` implication list in `packages/auth/src/rbac-canonical.ts`. No API contract changed and no compatibility layer was touched. Checked downstream consumer usage through `packages/app-shared/src/tests/permission.gate.test.tsx`. Verified with `npx vitest run packages/auth/src/__tests__/ability-implications.test.ts packages/auth/src/__tests__/rbac-canonical.test.ts packages/auth/src/__tests__/permissionGrouping.test.ts packages/app-shared/src/tests/permission.gate.test.tsx`.
```

### Structured Output

```json
{
  "bug_file": "/private/tmp/benchmark-bug-fix-002/control-cto-off/packages/auth/src/rbac-canonical.ts",
  "root_cause": "`PERMISSION_IMPLICATIONS[Action.MANAGE]` was missing `Action.SHARE`, so `actionImplies('manage', 'share')`, `getImpliedActions('manage')`, and `buildAbilityFromCapabilities(['manage:...'])` all failed to grant share access.",
  "fix_applied": true,
  "fix_description": "Restored `Action.SHARE` to the canonical `manage` implication list in `packages/auth/src/rbac-canonical.ts`. No API contract changed and no compatibility layer was touched. Checked downstream consumer usage through `packages/app-shared/src/tests/permission.gate.test.tsx`. Verified with `npx vitest run packages/auth/src/__tests__/ability-implications.test.ts packages/auth/src/__tests__/rbac-canonical.test.ts packages/auth/src/__tests__/permissionGrouping.test.ts packages/app-shared/src/tests/permission.gate.test.tsx`."
}
```

### Raw Run Record

```json
{
  "command": "codex exec -s danger-full-access --output-schema /tmp/aethyme-eval-output-schema.json -o /tmp/aethyme-eval-control-cto-off-result.json - < /tmp/aethyme-eval-control-cto-off-prompt.txt",
  "exit_code": 0,
  "input_tokens": 56947,
  "output_tokens": 0,
  "retries": 0,
  "review_burden": 0,
  "duration_seconds": 167,
  "structured_output": {
    "bug_file": "/private/tmp/benchmark-bug-fix-002/control-cto-off/packages/auth/src/rbac-canonical.ts",
    "root_cause": "`PERMISSION_IMPLICATIONS[Action.MANAGE]` was missing `Action.SHARE`, so `actionImplies('manage', 'share')`, `getImpliedActions('manage')`, and `buildAbilityFromCapabilities(['manage:...'])` all failed to grant share access.",
    "fix_applied": true,
    "fix_description": "Restored `Action.SHARE` to the canonical `manage` implication list in `packages/auth/src/rbac-canonical.ts`. No API contract changed and no compatibility layer was touched. Checked downstream consumer usage through `packages/app-shared/src/tests/permission.gate.test.tsx`. Verified with `npx vitest run packages/auth/src/__tests__/ability-implications.test.ts packages/auth/src/__tests__/rbac-canonical.test.ts packages/auth/src/__tests__/permissionGrouping.test.ts packages/app-shared/src/tests/permission.gate.test.tsx`."
  },
  "stdout": "{\"bug_file\": \"/private/tmp/benchmark-bug-fix-002/control-cto-off/packages/auth/src/rbac-canonical.ts\", \"root_cause\": \"`PERMISSION_IMPLICATIONS[Action.MANAGE]` was missing `Action.SHARE`, so `actionImplies('manage', 'share')`, `getImpliedActions('manage')`, and `buildAbilityFromCapabilities(['manage:...'])` all failed to grant share access.\", \"fix_applied\": true, \"fix_description\": \"Restored `Action.SHARE` to the canonical `manage` implication list in `packages/auth/src/rbac-canonical.ts`. No API contract changed and no compatibility layer was touched. Checked downstream consumer usage through `packages/app-shared/src/tests/permission.gate.test.tsx`. Verified with `npx vitest run packages/auth/src/__tests__/ability-implications.test.ts packages/auth/src/__tests__/rbac-canonical.test.ts packages/auth/src/__tests__/permissionGrouping.test.ts packages/app-shared/src/tests/permission.gate.test.tsx`.\"}",
  "final_output_message": "Restored `Action.SHARE` to the canonical `manage` implication list in `packages/auth/src/rbac-canonical.ts`. No API contract changed and no compatibility layer was touched. Checked downstream consumer usage through `packages/app-shared/src/tests/permission.gate.test.tsx`. Verified with `npx vitest run packages/auth/src/__tests__/ability-implications.test.ts packages/auth/src/__tests__/rbac-canonical.test.ts packages/auth/src/__tests__/permissionGrouping.test.ts packages/app-shared/src/tests/permission.gate.test.tsx`."
}
```

### Assessment

```json
{
  "scores": {
    "fix_test": 1.0,
    "regression": 1.0,
    "correct_file": 1.0,
    "efficiency": 0.7
  },
  "weighted_score": 97.0,
  "max_score": 100,
  "test_pass": true,
  "regression_pass": true,
  "tokens_used": 56947,
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 1,
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 97.0,
    "normalized_score": 97.0,
    "normalization_delta": 0.0
  }
}
```

## Control (CTO on)

### Prompt

```text
A test is failing in this repository.

Repository path: /private/tmp/benchmark-bug-fix-002/control-cto-on
Test file: packages/auth/src/__tests__/ability-implications.test.ts
Run command: npx vitest run packages/auth/src/__tests__/ability-implications.test.ts

Failure output:
FAIL packages/auth/src/__tests__/ability-implications.test.ts
  x permission implications > manage:suppliers grants share permission via ability builder
    -> expected true to be false
  x permission implications > manage:suppliers grants all expected permissions
    -> share check failed
  x permission implications > getImpliedActions for manage includes share
    -> expected array to contain 'share'
  x permission implications > actionImplies correctly checks manage -> share
    -> expected false to be true

The test file is correct. The bug is in the source code.
Find the root cause and fix the bug so all tests pass.
You can verify your fix by running the test command above.
```

### Run Metrics

- command: `codex exec -s danger-full-access --output-schema /tmp/aethyme-eval-output-schema.json -o /tmp/aethyme-eval-control-cto-on-result.json - < /tmp/aethyme-eval-control-cto-on-prompt.txt`
- exit code: `0`
- input tokens: `66643`
- output tokens: `0`
- retries: `0`
- review burden: `0`
- wall time: `167.000s`

### Final Output Message

```text
Re-added `Action.SHARE` to the `Action.MANAGE` implication list in `packages/auth/src/rbac-canonical.ts`. Verified with `npx vitest run packages/auth/src/__tests__/ability-implications.test.ts`, which now passes all 4 tests. Also checked a downstream consumer at `/private/tmp/benchmark-bug-fix-002/control-cto-on/packages/app-shared/src/hooks/chrome/useSessionProfile.ts` to confirm this shared auth contract still flows through ability building.
```

### Structured Output

```json
{
  "bug_file": "/private/tmp/benchmark-bug-fix-002/control-cto-on/packages/auth/src/rbac-canonical.ts",
  "root_cause": "`PERMISSION_IMPLICATIONS` no longer listed `Action.SHARE` under `Action.MANAGE`, so `actionImplies('manage', 'share')`, `getImpliedActions('manage')`, and CASL ability expansion for `manage:*` all dropped share access.",
  "fix_applied": true,
  "fix_description": "Re-added `Action.SHARE` to the `Action.MANAGE` implication list in `packages/auth/src/rbac-canonical.ts`. Verified with `npx vitest run packages/auth/src/__tests__/ability-implications.test.ts`, which now passes all 4 tests. Also checked a downstream consumer at `/private/tmp/benchmark-bug-fix-002/control-cto-on/packages/app-shared/src/hooks/chrome/useSessionProfile.ts` to confirm this shared auth contract still flows through ability building."
}
```

### Raw Run Record

```json
{
  "command": "codex exec -s danger-full-access --output-schema /tmp/aethyme-eval-output-schema.json -o /tmp/aethyme-eval-control-cto-on-result.json - < /tmp/aethyme-eval-control-cto-on-prompt.txt",
  "exit_code": 0,
  "input_tokens": 66643,
  "output_tokens": 0,
  "retries": 0,
  "review_burden": 0,
  "duration_seconds": 167,
  "structured_output": {
    "bug_file": "/private/tmp/benchmark-bug-fix-002/control-cto-on/packages/auth/src/rbac-canonical.ts",
    "root_cause": "`PERMISSION_IMPLICATIONS` no longer listed `Action.SHARE` under `Action.MANAGE`, so `actionImplies('manage', 'share')`, `getImpliedActions('manage')`, and CASL ability expansion for `manage:*` all dropped share access.",
    "fix_applied": true,
    "fix_description": "Re-added `Action.SHARE` to the `Action.MANAGE` implication list in `packages/auth/src/rbac-canonical.ts`. Verified with `npx vitest run packages/auth/src/__tests__/ability-implications.test.ts`, which now passes all 4 tests. Also checked a downstream consumer at `/private/tmp/benchmark-bug-fix-002/control-cto-on/packages/app-shared/src/hooks/chrome/useSessionProfile.ts` to confirm this shared auth contract still flows through ability building."
  },
  "stdout": "{\"bug_file\": \"/private/tmp/benchmark-bug-fix-002/control-cto-on/packages/auth/src/rbac-canonical.ts\", \"root_cause\": \"`PERMISSION_IMPLICATIONS` no longer listed `Action.SHARE` under `Action.MANAGE`, so `actionImplies('manage', 'share')`, `getImpliedActions('manage')`, and CASL ability expansion for `manage:*` all dropped share access.\", \"fix_applied\": true, \"fix_description\": \"Re-added `Action.SHARE` to the `Action.MANAGE` implication list in `packages/auth/src/rbac-canonical.ts`. Verified with `npx vitest run packages/auth/src/__tests__/ability-implications.test.ts`, which now passes all 4 tests. Also checked a downstream consumer at `/private/tmp/benchmark-bug-fix-002/control-cto-on/packages/app-shared/src/hooks/chrome/useSessionProfile.ts` to confirm this shared auth contract still flows through ability building.\"}",
  "final_output_message": "Re-added `Action.SHARE` to the `Action.MANAGE` implication list in `packages/auth/src/rbac-canonical.ts`. Verified with `npx vitest run packages/auth/src/__tests__/ability-implications.test.ts`, which now passes all 4 tests. Also checked a downstream consumer at `/private/tmp/benchmark-bug-fix-002/control-cto-on/packages/app-shared/src/hooks/chrome/useSessionProfile.ts` to confirm this shared auth contract still flows through ability building."
}
```

### Assessment

```json
{
  "scores": {
    "fix_test": 1.0,
    "regression": 1.0,
    "correct_file": 1.0,
    "efficiency": 0.4
  },
  "weighted_score": 94.0,
  "max_score": 100,
  "test_pass": true,
  "regression_pass": true,
  "tokens_used": 66643,
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 1,
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 94.0,
    "normalized_score": 94.0,
    "normalization_delta": 0.0
  }
}
```

## Explore

### Prompt

```text
A test is failing in this repository.

Repository path: /private/tmp/benchmark-bug-fix-002/explore
Test file: packages/auth/src/__tests__/ability-implications.test.ts
Run command: npx vitest run packages/auth/src/__tests__/ability-implications.test.ts

Failure output:
FAIL packages/auth/src/__tests__/ability-implications.test.ts
  x permission implications > manage:suppliers grants share permission via ability builder
    -> expected true to be false
  x permission implications > manage:suppliers grants all expected permissions
    -> share check failed
  x permission implications > getImpliedActions for manage includes share
    -> expected array to contain 'share'
  x permission implications > actionImplies correctly checks manage -> share
    -> expected false to be true

The test file is correct. The bug is in the source code.
Find the root cause and fix the bug so all tests pass.
You can verify your fix by running the test command above.
```

### Run Metrics

- command: `codex exec -s danger-full-access --output-schema /tmp/aethyme-eval-output-schema.json -o /tmp/aethyme-eval-explore-result.json - < /tmp/aethyme-eval-explore-prompt.txt`
- exit code: `0`
- input tokens: `98768`
- output tokens: `0`
- retries: `0`
- review burden: `0`
- wall time: `167.000s`

### Final Output Message

```text
Restored `Action.SHARE` to the `Action.MANAGE` implication list in `packages/auth/src/rbac-canonical.ts`, matching the backend canonical definition again. Verified with `npx vitest run packages/auth/src/__tests__/ability-implications.test.ts`, which now passes all 4 tests.
```

### Structured Output

```json
{
  "bug_file": "/private/tmp/benchmark-bug-fix-002/explore/packages/auth/src/rbac-canonical.ts",
  "root_cause": "`packages/auth/src/rbac-canonical.ts` had dropped `Action.SHARE` from `PERMISSION_IMPLICATIONS[Action.MANAGE]`. That broke the shared contract used by `getImpliedActions`, `actionImplies`, and the `buildAbilityFromCapabilities` consumer in `packages/auth/src/ability.ts`, so `manage:suppliers` no longer granted `share`.",
  "fix_applied": true,
  "fix_description": "Restored `Action.SHARE` to the `Action.MANAGE` implication list in `packages/auth/src/rbac-canonical.ts`, matching the backend canonical definition again. Verified with `npx vitest run packages/auth/src/__tests__/ability-implications.test.ts`, which now passes all 4 tests."
}
```

### Raw Run Record

```json
{
  "command": "codex exec -s danger-full-access --output-schema /tmp/aethyme-eval-output-schema.json -o /tmp/aethyme-eval-explore-result.json - < /tmp/aethyme-eval-explore-prompt.txt",
  "exit_code": 0,
  "input_tokens": 98768,
  "output_tokens": 0,
  "retries": 0,
  "review_burden": 0,
  "duration_seconds": 167,
  "structured_output": {
    "bug_file": "/private/tmp/benchmark-bug-fix-002/explore/packages/auth/src/rbac-canonical.ts",
    "root_cause": "`packages/auth/src/rbac-canonical.ts` had dropped `Action.SHARE` from `PERMISSION_IMPLICATIONS[Action.MANAGE]`. That broke the shared contract used by `getImpliedActions`, `actionImplies`, and the `buildAbilityFromCapabilities` consumer in `packages/auth/src/ability.ts`, so `manage:suppliers` no longer granted `share`.",
    "fix_applied": true,
    "fix_description": "Restored `Action.SHARE` to the `Action.MANAGE` implication list in `packages/auth/src/rbac-canonical.ts`, matching the backend canonical definition again. Verified with `npx vitest run packages/auth/src/__tests__/ability-implications.test.ts`, which now passes all 4 tests."
  },
  "stdout": "{\"bug_file\": \"/private/tmp/benchmark-bug-fix-002/explore/packages/auth/src/rbac-canonical.ts\", \"root_cause\": \"`packages/auth/src/rbac-canonical.ts` had dropped `Action.SHARE` from `PERMISSION_IMPLICATIONS[Action.MANAGE]`. That broke the shared contract used by `getImpliedActions`, `actionImplies`, and the `buildAbilityFromCapabilities` consumer in `packages/auth/src/ability.ts`, so `manage:suppliers` no longer granted `share`.\", \"fix_applied\": true, \"fix_description\": \"Restored `Action.SHARE` to the `Action.MANAGE` implication list in `packages/auth/src/rbac-canonical.ts`, matching the backend canonical definition again. Verified with `npx vitest run packages/auth/src/__tests__/ability-implications.test.ts`, which now passes all 4 tests.\"}",
  "final_output_message": "Restored `Action.SHARE` to the `Action.MANAGE` implication list in `packages/auth/src/rbac-canonical.ts`, matching the backend canonical definition again. Verified with `npx vitest run packages/auth/src/__tests__/ability-implications.test.ts`, which now passes all 4 tests."
}
```

### Assessment

```json
{
  "scores": {
    "fix_test": 1.0,
    "regression": 1.0,
    "correct_file": 1.0,
    "efficiency": 0.4
  },
  "weighted_score": 94.0,
  "max_score": 100,
  "test_pass": true,
  "regression_pass": true,
  "tokens_used": 98768,
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 1,
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 94.0,
    "normalized_score": 94.0,
    "normalization_delta": 0.0
  }
}
```

## Leverage

### Prompt

```text
Use Aethyme tools to navigate the repository graph.
Navigation context is available at /tmp/aethyme-eval-navigation-context.json

A test is failing in this repository.

Repository path: /tmp/benchmark-bug-fix-002/leverage
Test file: packages/auth/src/__tests__/ability-implications.test.ts
Run command: npx vitest run packages/auth/src/__tests__/ability-implications.test.ts

Failure output:
FAIL packages/auth/src/__tests__/ability-implications.test.ts
  x permission implications > manage:suppliers grants share permission via ability builder
    -> expected true to be false
  x permission implications > manage:suppliers grants all expected permissions
    -> share check failed
  x permission implications > getImpliedActions for manage includes share
    -> expected array to contain 'share'
  x permission implications > actionImplies correctly checks manage -> share
    -> expected false to be true

The test file is correct. The bug is in the source code.
Find the root cause and fix the bug so all tests pass.
You can verify your fix by running the test command above.
```

### Run Metrics

- command: `codex exec -s danger-full-access --output-schema /tmp/aethyme-eval-output-schema.json -o /tmp/aethyme-eval-leverage-result.json - < /tmp/aethyme-eval-leverage-prompt.txt`
- exit code: `0`
- input tokens: `78591`
- output tokens: `0`
- retries: `0`
- review burden: `0`
- wall time: `167.000s`

### Final Output Message

```text
Added `Action.SHARE` to the `Action.MANAGE` implications in `packages/auth/src/rbac-canonical.ts`. Verified by running `npx vitest run packages/auth/src/__tests__/ability-implications.test.ts`, which now passes all 4 tests.
```

### Structured Output

```json
{
  "bug_file": "/private/tmp/benchmark-bug-fix-002/leverage/packages/auth/src/rbac-canonical.ts",
  "root_cause": "The frontend RBAC mirror's `PERMISSION_IMPLICATIONS` table omitted `Action.SHARE` from the `Action.MANAGE` implication set, so `getImpliedActions('manage')` and `actionImplies('manage', 'share')` disagreed with the backend canonical contract.",
  "fix_applied": true,
  "fix_description": "Added `Action.SHARE` to the `Action.MANAGE` implications in `packages/auth/src/rbac-canonical.ts`. Verified by running `npx vitest run packages/auth/src/__tests__/ability-implications.test.ts`, which now passes all 4 tests."
}
```

### Raw Run Record

```json
{
  "command": "codex exec -s danger-full-access --output-schema /tmp/aethyme-eval-output-schema.json -o /tmp/aethyme-eval-leverage-result.json - < /tmp/aethyme-eval-leverage-prompt.txt",
  "exit_code": 0,
  "input_tokens": 78591,
  "output_tokens": 0,
  "retries": 0,
  "review_burden": 0,
  "duration_seconds": 167,
  "structured_output": {
    "bug_file": "/private/tmp/benchmark-bug-fix-002/leverage/packages/auth/src/rbac-canonical.ts",
    "root_cause": "The frontend RBAC mirror's `PERMISSION_IMPLICATIONS` table omitted `Action.SHARE` from the `Action.MANAGE` implication set, so `getImpliedActions('manage')` and `actionImplies('manage', 'share')` disagreed with the backend canonical contract.",
    "fix_applied": true,
    "fix_description": "Added `Action.SHARE` to the `Action.MANAGE` implications in `packages/auth/src/rbac-canonical.ts`. Verified by running `npx vitest run packages/auth/src/__tests__/ability-implications.test.ts`, which now passes all 4 tests."
  },
  "stdout": "{\"bug_file\": \"/private/tmp/benchmark-bug-fix-002/leverage/packages/auth/src/rbac-canonical.ts\", \"root_cause\": \"The frontend RBAC mirror's `PERMISSION_IMPLICATIONS` table omitted `Action.SHARE` from the `Action.MANAGE` implication set, so `getImpliedActions('manage')` and `actionImplies('manage', 'share')` disagreed with the backend canonical contract.\", \"fix_applied\": true, \"fix_description\": \"Added `Action.SHARE` to the `Action.MANAGE` implications in `packages/auth/src/rbac-canonical.ts`. Verified by running `npx vitest run packages/auth/src/__tests__/ability-implications.test.ts`, which now passes all 4 tests.\"}",
  "final_output_message": "Added `Action.SHARE` to the `Action.MANAGE` implications in `packages/auth/src/rbac-canonical.ts`. Verified by running `npx vitest run packages/auth/src/__tests__/ability-implications.test.ts`, which now passes all 4 tests."
}
```

### Assessment

```json
{
  "scores": {
    "fix_test": 1.0,
    "regression": 1.0,
    "correct_file": 1.0,
    "efficiency": 0.4
  },
  "weighted_score": 94.0,
  "max_score": 100,
  "test_pass": true,
  "regression_pass": true,
  "tokens_used": 78591,
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 1,
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 94.0,
    "normalized_score": 94.0,
    "normalization_delta": 0.0
  }
}
```


## Context Pack Audit

### Navigation Context

```json
{
  "mode": "bug_fix_navigation",
  "repo_path": "/tmp/benchmark-bug-fix-002/leverage",
  "task": "Fix failing test: manage permission does not imply share in ability-implications.test.ts",
  "test_file": "packages/auth/src/__tests__/ability-implications.test.ts",
  "bug_area": "packages/auth/src/",
  "anchors": [
    {
      "kind": "file",
      "id": "packages/auth/src/__tests__/ability-implications.test.ts",
      "file": "packages/auth/src/__tests__/ability-implications.test.ts",
      "reason": "file reference from task text (ability-implications.test.ts)"
    },
    {
      "kind": "folder",
      "id": "packages",
      "file": null,
      "reason": "area containing referenced file (ability-implications.test.ts)"
    }
  ],
  "scope": {
    "task": "Fix failing test: manage permission does not imply share in ability-implications.test.ts",
    "navigation_order": [
      "packages/auth/src/__tests__/ability-implications.test.ts",
      "packages"
    ],
    "in_scope_files": [
      "packages/auth/src/__tests__/ability-implications.test.ts"
    ],
    "in_scope_symbols": [],
    "in_scope_areas": [
      "packages"
    ],
    "out_of_scope": [
      ".githooks",
      ".github",
      ".github/PULL_REQUEST_TEMPLATE",
      ".github/workflows",
      ".husky",
      ".lighthouseci",
      ".playwright-mcp",
      ".pnpm-store",
      ".storybook",
      "Agents",
      "Agents/Skills Manager",
      "Agents/skills",
      "TODO",
      "alerts",
      "apps",
      "apps/customer",
      "apps/mordor",
      "apps/organizations",
      "backend",
      "backend/accounts",
      "backend/adn",
      "backend/aep_backend",
      "backend/ai_providers",
      "backend/analytics",
      "backend/api_keys",
      "backend/api_usage",
      "backend/audit",
      "backend/automations",
      "backend/collaboration",
      "backend/common",
      "backend/community",
      "backend/controls",
      "backend/core",
      "backend/directory",
      "backend/docs",
      "backend/documents",
      "backend/environment",
      "backend/events",
      "backend/frameworks",
      "backend/information",
      "backend/integrations",
      "backend/k8s",
      "backend/knowledge",
      "backend/localization",
      "backend/mapping_intelligence",
      "backend/menu_overrides",
      "backend/middleware",
      "backend/onboarding",
      "backend/operational",
      "backend/page_actions",
      "backend/posture",
      "backend/project",
      "backend/reports",
      "backend/scripts",
      "backend/tasks",
      "backend/templates",
      "backend/tests",
      "backend/thirdparties",
      "backend/webhooks",
      "catalog",
      "config",
      "config/bundle",
      "config/lighthouse",
      "config/observability",
      "config/quality",
      "contracts",
      "contracts/config",
      "devops",
      "docker",
      "docs",
      "docs/adr",
      "docs/agents",
      "docs/api",
      "docs/architecture",
      "docs/badges",
      "docs/collaboration",
      "docs/contracts",
      "docs/db",
      "docs/design-system",
      "docs/development",
      "docs/docker",
      "docs/engineering",
      "docs/feature-specs",
      "docs/guides",
      "docs/observability",
      "docs/onboarding",
      "docs/openapi",
      "docs/otlp",
      "docs/performance",
      "docs/planning",
      "docs/plans",
      "docs/prd",
      "docs/reference",
      "docs/runbooks",
      "docs/security",
      "docs/testing",
      "e2e",
      "e2e/fixtures",
      "e2e/page-objects",
      "functions",
      "gcp-run-proxy",
      "gcp-run-proxy/src",
      "gcp-run-proxy/test",
      "grafana-provisioning",
      "load_tests",
      "output",
      "packages/app-shared",
      "packages/auth",
      "packages/config",
      "packages/eslint-plugin-aeptus",
      "packages/types",
      "packages/ui",
      "patches",
      "project",
      "public",
      "scripts",
      "scripts/a11y",
      "scripts/adr",
      "scripts/ai",
      "scripts/archive",
      "scripts/assets",
      "scripts/catalog",
      "scripts/checks",
      "scripts/ci",
      "scripts/contracts",
      "scripts/design-system",
      "scripts/dev",
      "scripts/docs",
      "scripts/generate",
      "scripts/help",
      "scripts/i18n",
      "scripts/k6",
      "scripts/maintenance",
      "scripts/naming",
      "scripts/observability",
      "scripts/openapi",
      "scripts/perf",
      "scripts/security",
      "scripts/tools",
      "scripts/trace",
      "scripts/validation",
      "scripts/ws",
      "shared",
      "src",
      "src/i18n",
      "stories",
      "tests",
      "tests/contract",
      "tools",
      "tools/mcp-mordor"
    ]
  },
  "file_contents": {}
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

| Metric | Control (CTO off) | Control (CTO on) | Explore | Leverage |
| --- | --- | --- | --- | --- |
| Prompt chars | `964` | `963` | `956` | `1079` |
| Score | `97.0` | `94.0` | `94.0` | `94.0` |

- Navigation items surfaced: `0`
- Risk items surfaced: `0`

## Reference

### Output Schema

```json
{
  "type": "object",
  "additionalProperties": false,
  "required": [
    "bug_file",
    "root_cause",
    "fix_applied",
    "fix_description"
  ],
  "properties": {
    "bug_file": {
      "type": "string",
      "description": "Path to the file containing the bug"
    },
    "root_cause": {
      "type": "string",
      "description": "What caused the test to fail"
    },
    "fix_applied": {
      "type": "boolean",
      "description": "Did you modify a file to fix the bug?"
    },
    "fix_description": {
      "type": "string",
      "description": "What change did you make?"
    }
  }
}
```

### Scoring Rubric

```json
{
  "weights": {
    "fix_test": 60,
    "regression": 20,
    "correct_file": 10,
    "efficiency": 10
  },
  "notes": [
    "fix_test: vitest run on the planted test \u2014 binary pass/fail.",
    "regression: vitest run on all auth tests \u2014 no regressions introduced.",
    "correct_file: agent identifies the correct file in structured output.",
    "efficiency: lower token usage scores higher."
  ]
}
```

### Reference Output

```json
{
  "bug_file": "packages/auth/src/rbac-canonical.ts",
  "bug_line": "    Action.SHARE,",
  "fix": "Add Action.SHARE back to PERMISSION_IMPLICATIONS[manage] array",
  "root_cause": "Action.SHARE was removed from the manage permission implications",
  "fix_applied": true,
  "fix_description": "Restored Action.SHARE to the PERMISSION_IMPLICATIONS[manage] array in rbac-canonical.ts"
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
