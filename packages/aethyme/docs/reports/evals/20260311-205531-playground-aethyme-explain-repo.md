# Eval Report: Explain this repo

Last Updated: 2026-03-11

- Repository: `/Users/christophehenner/Downloads/Repositories/Playground Aethyme`
- Generated: `2026-03-11T20:55:31.869105+00:00`
- Conditions: `control-cto-off, control-cto-on, control, explore, leverage`

## Summary

- Control (CTO off) prompt chars: `167`
- Control (CTO on) prompt chars: `167`
- Explore prompt chars: `167`
- Leverage prompt chars: `219`
- Navigation items: `5`
- Risk items: `0`

### Scorecard

| Condition | Score | Tokens | Tool Calls |
| --- | --- | --- | --- |
| Control (CTO off) | 10.0 | 30K | - |
| Control (CTO on) | 5.0 | 121K | - |
| Control | - | - | - |
| Explore | 5.0 | 153K | - |
| Leverage | 5.0 | 169K | - |

## Repo Signals

```json
{
  "boundary_clarity": {
    "score": 68,
    "level": "mixed",
    "evidence": [
      "cross-area semantic edges: 41610/264763",
      "source files with area assignment: 5401/5420",
      "generic source file names: 13"
    ]
  },
  "entrypoint_clarity": {
    "score": 100,
    "level": "strong",
    "evidence": [
      "direct code entrypoint edges: 1197",
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
      "low-confidence semantic edges: 209685/236039",
      "high-confidence semantic edges: 14717/236039",
      "cross-area semantic edges: 31018/236039"
    ]
  },
  "parser_visibility": {
    "score": 87,
    "level": "strong",
    "evidence": [
      "supported source files: 5130/5420",
      "source files with semantic extraction: 3828/5420",
      "total extracted functions/classes: 16089"
    ]
  }
}
```

## Control (CTO off)

### Prompt

```text
Task: Explain this repo
Repository path: /Users/christophehenner/Downloads/Repositories/Playground Aethyme
Explore the repository and produce a structured explanation.
```

### Run Metrics

- command: `codex exec (via Chau7 tab)`
- exit code: `0`
- input tokens: `30447`
- output tokens: `0`
- retries: `0`
- review burden: `0`
- wall time: `0.000s`

### Final Output Message

```text
Aeptus is a large, docs-heavy monorepo for a multi-tenant GRC platform. The core product idea is “machine-first GRC”: automate governance/risk/compliance work, then let humans review decisions. The repo combines three React SPAs, a Django backend, shared frontend packages, extensive documentation/generation tooling, operational scripts, and AI-agent support.
```

### Structured Output

```json
{
  "repo_summary": "Aeptus is a large, docs-heavy monorepo for a multi-tenant GRC platform. The core product idea is \u201cmachine-first GRC\u201d: automate governance/risk/compliance work, then let humans review decisions. The repo combines three React SPAs, a Django backend, shared frontend packages, extensive documentation/generation tooling, operational scripts, and AI-agent support.",
  "code_areas": [
    "Frontend apps: `apps/customer`, `apps/mordor`, `apps/organizations`",
    "Shared frontend core: `packages/app-shared`",
    "Design system: `packages/ui`",
    "Shared config/types/auth: `packages/config`, `packages/types`, `packages/auth`",
    "Backend domains: `backend/*` Django apps",
    "Operational/deployment/tooling: `scripts`, `docker`, `devops`, `.github/workflows`, `tools/mcp-mordor`",
    "Documentation/agent systems: `docs`, `Agents`"
  ],
  "reference_areas": [
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/README.md",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/architecture/architecture-summary.md",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/README.md",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/onboarding/quick-reference.md",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/README.md",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/tools/mcp-mordor/README.md"
  ],
  "entrypoints": [
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/customer/src/main.tsx",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/customer/src/entry-authenticated.tsx",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/mordor/src/main.tsx",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/organizations/src/main.tsx",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/app/Bootstrap.tsx",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/router/index.tsx",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/aep_backend/urls.py",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/aep_backend/settings.py"
  ],
  "important_docs": [
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/AGENTS.md",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/README.md",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/architecture/frontend-architecture.md",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/architecture/backend-architecture.md",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/runbooks/README.md",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/testing/README.md"
  ],
  "key_configs": [
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/package.json",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/pnpm-workspace.yaml",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docker-compose.yml",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/vitest.config.ts",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/playwright.config.ts",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/pyproject.toml"
  ],
  "key_languages": [
    "TypeScript",
    "React",
    "Python",
    "Django",
    "CSS/Tailwind",
    "Shell"
  ],
  "high_risk_areas": [
    "`packages/app-shared/src/menu.config.ts` and router/view registry coupling drive all apps",
    "`backend/aep_backend/settings.py` and `backend/aep_backend/urls.py` centralize auth, middleware, and endpoint wiring",
    "Shared packages (`packages/ui`, `packages/app-shared`) can break multiple apps at once",
    "Docs/tooling drift is a risk because the repo has a very large generated/manual documentation surface",
    "Operational scripts in root `package.json` and `scripts/` are central to builds and validation"
  ],
  "navigation_order": [
    "Start with README and architecture summary",
    "Read root workspace config and scripts",
    "Inspect the three app entrypoints",
    "Inspect `packages/app-shared` bootstrap/router/config",
    "Inspect shared UI/config/types packages",
    "Inspect backend settings and URL wiring",
    "Then inspect domain-specific backend apps and docs/runbooks"
  ],
  "representative_code_files": [
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/customer/src/main.tsx",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/customer/menu.config.ts",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/mordor/menu.config.ts",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/organizations/menu.config.ts",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/app/Bootstrap.tsx",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/app/AuthenticatedApp.tsx",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/config/schema.ts",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/router/BuildRouter.tsx",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/aep_backend/settings.py",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/aep_backend/urls.py"
  ],
  "representative_docs": [
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/README.md",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/architecture/architecture-summary.md",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/architecture/frontend-architecture.md",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/README.md",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/onboarding/quick-reference.md",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/tools/mcp-mordor/README.md"
  ],
  "evidence": [
    "Root README describes Aeptus as an enterprise GRC platform with React/TypeScript/Django/PostgreSQL and a machine-first operating model.",
    "Workspace config shows a pnpm monorepo with `apps/*`, `packages/*`, `tools/*`, and `gcp-run-proxy`.",
    "Root package.json contains the main orchestration surface for dev/build/test/docs/security commands.",
    "Frontend entrypoints use a shared pre-auth loader before mounting the authenticated React app.",
    "`packages/app-shared` owns bootstrap, providers, router construction, and route schema.",
    "Shared routing is config-driven: routes are declared as data and filtered by scope (`tenant`, `org`, `staff`, `shared`).",
    "The shared menu file contains large route families for suppliers, environment, IMS, risk, controls, reporting, admin, platform, and automations.",
    "Backend settings register many Django apps, optional Channels support, Celery/Redis integration, and extensive middleware.",
    "Backend URL config exposes auth, admin, notifications, audit, and API surfaces from a single project router.",
    "Docker Compose provisions postgres, redis, pgbouncer, django web, celery, and observability-related services.",
    "The repo has strong emphasis on testing, docs generation, data-ui selectors, and CI workflow coverage.",
    "`tools/mcp-mordor` shows that the repo also supports AI/LLM-driven operational access into the platform via an MCP server."
  ]
}
```

### Raw Run Record

```json
{
  "structured_output": {
    "repo_summary": "Aeptus is a large, docs-heavy monorepo for a multi-tenant GRC platform. The core product idea is \u201cmachine-first GRC\u201d: automate governance/risk/compliance work, then let humans review decisions. The repo combines three React SPAs, a Django backend, shared frontend packages, extensive documentation/generation tooling, operational scripts, and AI-agent support.",
    "code_areas": [
      "Frontend apps: `apps/customer`, `apps/mordor`, `apps/organizations`",
      "Shared frontend core: `packages/app-shared`",
      "Design system: `packages/ui`",
      "Shared config/types/auth: `packages/config`, `packages/types`, `packages/auth`",
      "Backend domains: `backend/*` Django apps",
      "Operational/deployment/tooling: `scripts`, `docker`, `devops`, `.github/workflows`, `tools/mcp-mordor`",
      "Documentation/agent systems: `docs`, `Agents`"
    ],
    "reference_areas": [
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/README.md",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/architecture/architecture-summary.md",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/README.md",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/onboarding/quick-reference.md",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/README.md",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/tools/mcp-mordor/README.md"
    ],
    "entrypoints": [
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/customer/src/main.tsx",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/customer/src/entry-authenticated.tsx",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/mordor/src/main.tsx",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/organizations/src/main.tsx",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/app/Bootstrap.tsx",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/router/index.tsx",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/aep_backend/urls.py",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/aep_backend/settings.py"
    ],
    "important_docs": [
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/AGENTS.md",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/README.md",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/architecture/frontend-architecture.md",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/architecture/backend-architecture.md",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/runbooks/README.md",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/testing/README.md"
    ],
    "key_configs": [
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/package.json",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/pnpm-workspace.yaml",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docker-compose.yml",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/vitest.config.ts",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/playwright.config.ts",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/pyproject.toml"
    ],
    "key_languages": [
      "TypeScript",
      "React",
      "Python",
      "Django",
      "CSS/Tailwind",
      "Shell"
    ],
    "high_risk_areas": [
      "`packages/app-shared/src/menu.config.ts` and router/view registry coupling drive all apps",
      "`backend/aep_backend/settings.py` and `backend/aep_backend/urls.py` centralize auth, middleware, and endpoint wiring",
      "Shared packages (`packages/ui`, `packages/app-shared`) can break multiple apps at once",
      "Docs/tooling drift is a risk because the repo has a very large generated/manual documentation surface",
      "Operational scripts in root `package.json` and `scripts/` are central to builds and validation"
    ],
    "navigation_order": [
      "Start with README and architecture summary",
      "Read root workspace config and scripts",
      "Inspect the three app entrypoints",
      "Inspect `packages/app-shared` bootstrap/router/config",
      "Inspect shared UI/config/types packages",
      "Inspect backend settings and URL wiring",
      "Then inspect domain-specific backend apps and docs/runbooks"
    ],
    "representative_code_files": [
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/customer/src/main.tsx",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/customer/menu.config.ts",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/mordor/menu.config.ts",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/organizations/menu.config.ts",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/app/Bootstrap.tsx",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/app/AuthenticatedApp.tsx",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/config/schema.ts",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/router/BuildRouter.tsx",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/aep_backend/settings.py",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/aep_backend/urls.py"
    ],
    "representative_docs": [
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/README.md",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/architecture/architecture-summary.md",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/architecture/frontend-architecture.md",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/README.md",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/onboarding/quick-reference.md",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/tools/mcp-mordor/README.md"
    ],
    "evidence": [
      "Root README describes Aeptus as an enterprise GRC platform with React/TypeScript/Django/PostgreSQL and a machine-first operating model.",
      "Workspace config shows a pnpm monorepo with `apps/*`, `packages/*`, `tools/*`, and `gcp-run-proxy`.",
      "Root package.json contains the main orchestration surface for dev/build/test/docs/security commands.",
      "Frontend entrypoints use a shared pre-auth loader before mounting the authenticated React app.",
      "`packages/app-shared` owns bootstrap, providers, router construction, and route schema.",
      "Shared routing is config-driven: routes are declared as data and filtered by scope (`tenant`, `org`, `staff`, `shared`).",
      "The shared menu file contains large route families for suppliers, environment, IMS, risk, controls, reporting, admin, platform, and automations.",
      "Backend settings register many Django apps, optional Channels support, Celery/Redis integration, and extensive middleware.",
      "Backend URL config exposes auth, admin, notifications, audit, and API surfaces from a single project router.",
      "Docker Compose provisions postgres, redis, pgbouncer, django web, celery, and observability-related services.",
      "The repo has strong emphasis on testing, docs generation, data-ui selectors, and CI workflow coverage.",
      "`tools/mcp-mordor` shows that the repo also supports AI/LLM-driven operational access into the platform via an MCP server."
    ]
  },
  "input_tokens": 30447,
  "output_tokens": 0,
  "command": "codex exec (via Chau7 tab)",
  "exit_code": 0,
  "retries": 0,
  "review_burden": 0,
  "duration_seconds": 0,
  "final_output_message": "Aeptus is a large, docs-heavy monorepo for a multi-tenant GRC platform. The core product idea is \u201cmachine-first GRC\u201d: automate governance/risk/compliance work, then let humans review decisions. The repo combines three React SPAs, a Django backend, shared frontend packages, extensive documentation/generation tooling, operational scripts, and AI-agent support.",
  "stderr_size": 14838
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
      "absolute_count": 42,
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
Task: Explain this repo
Repository path: /Users/christophehenner/Downloads/Repositories/Playground Aethyme
Explore the repository and produce a structured explanation.
```

### Run Metrics

- command: `codex exec (via Chau7 tab)`
- exit code: `0`
- input tokens: `121525`
- output tokens: `0`
- retries: `0`
- review burden: `0`
- wall time: `0.000s`

### Final Output Message

```text
Aeptus is a large monorepo for a machine-first, multi-tenant GRC platform. It combines multiple React/TypeScript SPAs, a substantial Django backend, shared UI/config/auth packages, heavy docs/automation, and operational tooling such as a Cloudflare proxy and an MCP server for Mordor operations.
```

### Structured Output

```json
{
  "repo_summary": "Aeptus is a large monorepo for a machine-first, multi-tenant GRC platform. It combines multiple React/TypeScript SPAs, a substantial Django backend, shared UI/config/auth packages, heavy docs/automation, and operational tooling such as a Cloudflare proxy and an MCP server for Mordor operations.",
  "code_areas": [
    "Frontend apps in `apps/customer`, `apps/mordor`, and `apps/organizations`",
    "Shared frontend/runtime code in `packages/app-shared`",
    "Design system and shared libraries in `packages/ui`, `packages/auth`, `packages/config`, `packages/types`",
    "Django backend domains in `backend/*`",
    "Operational/tooling code in `gcp-run-proxy` and `tools/mcp-mordor`",
    "Testing and contracts in `e2e`, `tests/contract`, and `contracts`"
  ],
  "reference_areas": [
    "`README.md`",
    "`docs/architecture/architecture-summary.md`",
    "`backend/README.md`",
    "`docs/README.md`",
    "`docs/agents/context/README.md`",
    "`Agents/skills/product-vision/skill.md`"
  ],
  "entrypoints": [
    "`apps/customer/src/main.tsx`",
    "`apps/customer/src/entry-authenticated.tsx`",
    "`apps/mordor/src/main.tsx`",
    "`apps/mordor/src/App.tsx`",
    "`apps/organizations/src/main.tsx`",
    "`backend/aep_backend/settings.py`",
    "`backend/aep_backend/urls.py`",
    "`gcp-run-proxy/src/worker.mjs`",
    "`tools/mcp-mordor/src/index.ts`"
  ],
  "important_docs": [
    "`README.md`",
    "`docs/architecture/architecture-summary.md`",
    "`backend/README.md`",
    "`apps/mordor/README.md`",
    "`docs/README.md`",
    "`docs/agents/context/README.md`",
    "`e2e/README.md`",
    "`contracts/README.md`",
    "`tests/contract/README.md`"
  ],
  "key_configs": [
    "`package.json`",
    "`pnpm-workspace.yaml`",
    "`apps/customer/package.json`",
    "`apps/mordor/package.json`",
    "`apps/organizations/package.json`",
    "`packages/app-shared/package.json`",
    "`packages/config/package.json`",
    "`backend/pyproject.toml`"
  ],
  "key_languages": [
    "TypeScript",
    "React",
    "Python",
    "Django",
    "SQL",
    "Shell/Node automation"
  ],
  "high_risk_areas": [
    "`packages/app-shared` because most frontend behavior is centralized there",
    "`backend/aep_backend/settings.py` middleware/security stack",
    "`backend/*` domain apps because of tenant isolation, audit, and temporal data concerns",
    "Shared packages (`packages/ui`, `packages/auth`, `packages/config`, `packages/types`) because changes ripple across multiple apps",
    "Operational tooling (`gcp-run-proxy`, `tools/mcp-mordor`) because it touches auth/proxy/platform operations"
  ],
  "navigation_order": [
    "Open `README.md` for repo purpose and dev workflow",
    "Read `docs/architecture/architecture-summary.md` for the end-to-end system model",
    "Inspect `apps/customer/src/main.tsx` and `apps/customer/src/entry-authenticated.tsx` to see the frontend bootstrap pattern",
    "Inspect `packages/app-shared/src` to find the real shared routes/pages/features",
    "Read `apps/mordor/README.md` to understand the internal ops app",
    "Read `backend/README.md`, then `backend/aep_backend/settings.py` and `backend/aep_backend/urls.py` for backend composition",
    "Use `docs/README.md` and `docs/agents/context/README.md` when drilling into a domain",
    "Use `e2e/README.md`, `contracts/README.md`, and `tests/contract/README.md` for validation strategy"
  ],
  "representative_code_files": [
    "`apps/customer/src/main.tsx`",
    "`apps/customer/src/entry-authenticated.tsx`",
    "`apps/mordor/src/App.tsx`",
    "`packages/app-shared/package.json`",
    "`backend/aep_backend/settings.py`",
    "`backend/aep_backend/urls.py`",
    "`gcp-run-proxy/src/worker.mjs`",
    "`tools/mcp-mordor/src/index.ts`"
  ],
  "representative_docs": [
    "`README.md`",
    "`docs/architecture/architecture-summary.md`",
    "`backend/README.md`",
    "`apps/mordor/README.md`",
    "`docs/README.md`",
    "`docs/agents/context/README.md`"
  ],
  "evidence": [
    "`README.md` describes the repo as a config-driven GRC platform with React/TypeScript frontends and a Django/PostgreSQL backend",
    "`Agents/skills/product-vision/skill.md` defines the product philosophy as machine-first, decision-oriented GRC",
    "`docs/architecture/architecture-summary.md` describes Cloudflare Pages + Worker in front of Cloud Run + PostgreSQL/Redis",
    "`apps/customer/src/main.tsx` shows a pre-auth micro-entrypoint that loads the full app only after session validation",
    "`packages/app-shared` contains most of the shared frontend pages, features, hooks, providers, and router code",
    "`apps/mordor/README.md` explains Mordor as the internal vendor operations SPA, distinct from customer admin",
    "`backend/README.md` and `backend/aep_backend/settings.py` show a large Django backend with many domain apps, RLS, audit, and middleware",
    "`tools/mcp-mordor/README.md` shows the repo also exposes platform operations through an MCP server",
    "`e2e/README.md`, `contracts/README.md`, and `tests/contract/README.md` show layered testing: Playwright, Schemathesis, and Pact consumer contracts"
  ]
}
```

### Raw Run Record

```json
{
  "structured_output": {
    "repo_summary": "Aeptus is a large monorepo for a machine-first, multi-tenant GRC platform. It combines multiple React/TypeScript SPAs, a substantial Django backend, shared UI/config/auth packages, heavy docs/automation, and operational tooling such as a Cloudflare proxy and an MCP server for Mordor operations.",
    "code_areas": [
      "Frontend apps in `apps/customer`, `apps/mordor`, and `apps/organizations`",
      "Shared frontend/runtime code in `packages/app-shared`",
      "Design system and shared libraries in `packages/ui`, `packages/auth`, `packages/config`, `packages/types`",
      "Django backend domains in `backend/*`",
      "Operational/tooling code in `gcp-run-proxy` and `tools/mcp-mordor`",
      "Testing and contracts in `e2e`, `tests/contract`, and `contracts`"
    ],
    "reference_areas": [
      "`README.md`",
      "`docs/architecture/architecture-summary.md`",
      "`backend/README.md`",
      "`docs/README.md`",
      "`docs/agents/context/README.md`",
      "`Agents/skills/product-vision/skill.md`"
    ],
    "entrypoints": [
      "`apps/customer/src/main.tsx`",
      "`apps/customer/src/entry-authenticated.tsx`",
      "`apps/mordor/src/main.tsx`",
      "`apps/mordor/src/App.tsx`",
      "`apps/organizations/src/main.tsx`",
      "`backend/aep_backend/settings.py`",
      "`backend/aep_backend/urls.py`",
      "`gcp-run-proxy/src/worker.mjs`",
      "`tools/mcp-mordor/src/index.ts`"
    ],
    "important_docs": [
      "`README.md`",
      "`docs/architecture/architecture-summary.md`",
      "`backend/README.md`",
      "`apps/mordor/README.md`",
      "`docs/README.md`",
      "`docs/agents/context/README.md`",
      "`e2e/README.md`",
      "`contracts/README.md`",
      "`tests/contract/README.md`"
    ],
    "key_configs": [
      "`package.json`",
      "`pnpm-workspace.yaml`",
      "`apps/customer/package.json`",
      "`apps/mordor/package.json`",
      "`apps/organizations/package.json`",
      "`packages/app-shared/package.json`",
      "`packages/config/package.json`",
      "`backend/pyproject.toml`"
    ],
    "key_languages": [
      "TypeScript",
      "React",
      "Python",
      "Django",
      "SQL",
      "Shell/Node automation"
    ],
    "high_risk_areas": [
      "`packages/app-shared` because most frontend behavior is centralized there",
      "`backend/aep_backend/settings.py` middleware/security stack",
      "`backend/*` domain apps because of tenant isolation, audit, and temporal data concerns",
      "Shared packages (`packages/ui`, `packages/auth`, `packages/config`, `packages/types`) because changes ripple across multiple apps",
      "Operational tooling (`gcp-run-proxy`, `tools/mcp-mordor`) because it touches auth/proxy/platform operations"
    ],
    "navigation_order": [
      "Open `README.md` for repo purpose and dev workflow",
      "Read `docs/architecture/architecture-summary.md` for the end-to-end system model",
      "Inspect `apps/customer/src/main.tsx` and `apps/customer/src/entry-authenticated.tsx` to see the frontend bootstrap pattern",
      "Inspect `packages/app-shared/src` to find the real shared routes/pages/features",
      "Read `apps/mordor/README.md` to understand the internal ops app",
      "Read `backend/README.md`, then `backend/aep_backend/settings.py` and `backend/aep_backend/urls.py` for backend composition",
      "Use `docs/README.md` and `docs/agents/context/README.md` when drilling into a domain",
      "Use `e2e/README.md`, `contracts/README.md`, and `tests/contract/README.md` for validation strategy"
    ],
    "representative_code_files": [
      "`apps/customer/src/main.tsx`",
      "`apps/customer/src/entry-authenticated.tsx`",
      "`apps/mordor/src/App.tsx`",
      "`packages/app-shared/package.json`",
      "`backend/aep_backend/settings.py`",
      "`backend/aep_backend/urls.py`",
      "`gcp-run-proxy/src/worker.mjs`",
      "`tools/mcp-mordor/src/index.ts`"
    ],
    "representative_docs": [
      "`README.md`",
      "`docs/architecture/architecture-summary.md`",
      "`backend/README.md`",
      "`apps/mordor/README.md`",
      "`docs/README.md`",
      "`docs/agents/context/README.md`"
    ],
    "evidence": [
      "`README.md` describes the repo as a config-driven GRC platform with React/TypeScript frontends and a Django/PostgreSQL backend",
      "`Agents/skills/product-vision/skill.md` defines the product philosophy as machine-first, decision-oriented GRC",
      "`docs/architecture/architecture-summary.md` describes Cloudflare Pages + Worker in front of Cloud Run + PostgreSQL/Redis",
      "`apps/customer/src/main.tsx` shows a pre-auth micro-entrypoint that loads the full app only after session validation",
      "`packages/app-shared` contains most of the shared frontend pages, features, hooks, providers, and router code",
      "`apps/mordor/README.md` explains Mordor as the internal vendor operations SPA, distinct from customer admin",
      "`backend/README.md` and `backend/aep_backend/settings.py` show a large Django backend with many domain apps, RLS, audit, and middleware",
      "`tools/mcp-mordor/README.md` shows the repo also exposes platform operations through an MCP server",
      "`e2e/README.md`, `contracts/README.md`, and `tests/contract/README.md` show layered testing: Playwright, Schemathesis, and Pact consumer contracts"
    ]
  },
  "input_tokens": 121525,
  "output_tokens": 0,
  "command": "codex exec (via Chau7 tab)",
  "exit_code": 0,
  "retries": 0,
  "review_burden": 0,
  "duration_seconds": 0,
  "final_output_message": "Aeptus is a large monorepo for a machine-first, multi-tenant GRC platform. It combines multiple React/TypeScript SPAs, a substantial Django backend, shared UI/config/auth packages, heavy docs/automation, and operational tooling such as a Cloudflare proxy and an MCP server for Mordor operations.",
  "stderr_size": 78873
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
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 5.0,
    "normalized_score": 5.0,
    "normalization_delta": 0.0
  }
}
```

## Control

### Prompt

```text
Task: Explain this repo
Repository path: /Users/christophehenner/Downloads/Repositories/Playground Aethyme
Explore the repository and produce a structured explanation.
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
Task: Explain this repo
Repository path: /Users/christophehenner/Downloads/Repositories/Playground Aethyme
Explore the repository and produce a structured explanation.
```

### Run Metrics

- command: `codex exec (via Chau7 tab)`
- exit code: `0`
- input tokens: `153431`
- output tokens: `0`
- retries: `0`
- review burden: `0`
- wall time: `0.000s`

### Final Output Message

```text
Aeptus is a large monorepo for a machine-first GRC platform: three React SPAs on the frontend, a multi-tenant Django backend, a Cloudflare Worker edge proxy, and substantial documentation/agent/tooling around all of it. The dominant pattern is shared product logic in `packages/app-shared`, with the app folders acting mostly as thin shells for build target, route scope, and per-app view registry.
```

### Structured Output

```json
{
  "repo_summary": "Aeptus is a large monorepo for a machine-first GRC platform: three React SPAs on the frontend, a multi-tenant Django backend, a Cloudflare Worker edge proxy, and substantial documentation/agent/tooling around all of it. The dominant pattern is shared product logic in `packages/app-shared`, with the app folders acting mostly as thin shells for build target, route scope, and per-app view registry.",
  "code_areas": [
    "Frontend apps in `apps/customer`, `apps/mordor`, and `apps/organizations`",
    "Shared frontend product layer in `packages/app-shared/src`",
    "Design system in `packages/ui/src`",
    "Frontend auth/config/types packages in `packages/auth`, `packages/config`, `packages/types`",
    "Django backend in `backend/*` with many domain apps",
    "Cloudflare Worker proxy in `gcp-run-proxy/src/worker.mjs`",
    "Repo automation in `scripts/*` and AI/agent workflow in `Agents/*`"
  ],
  "reference_areas": [
    "`README.md`",
    "`docs/architecture/architecture-summary.md`",
    "`docs/architecture/structure.md`",
    "`Agents/skills/product-vision/skill.md`",
    "`backend/README.md`",
    "`ai-agent-onboarding.md`",
    "`e2e/README.md`"
  ],
  "entrypoints": [
    "`apps/customer/src/main.tsx`",
    "`apps/customer/src/entry-authenticated.tsx`",
    "`apps/mordor/src/main.tsx`",
    "`apps/organizations/src/main.tsx`",
    "`packages/app-shared/src/app/Bootstrap.tsx`",
    "`packages/app-shared/src/router/index.tsx`",
    "`backend/manage.py`",
    "`backend/aep_backend/asgi.py`",
    "`backend/aep_backend/urls.py`",
    "`gcp-run-proxy/src/worker.mjs`"
  ],
  "important_docs": [
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/README.md`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/architecture/architecture-summary.md`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/architecture/structure.md`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/README.md`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/ai-agent-onboarding.md`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/e2e/README.md`"
  ],
  "key_configs": [
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/package.json`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/pnpm-workspace.yaml`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/vite.config.mjs`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/customer/vite.config.ts`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/mordor/vite.config.ts`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/organizations/vite.config.ts`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/aep_backend/settings.py`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/wrangler.toml`"
  ],
  "key_languages": [
    "TypeScript",
    "React",
    "Python",
    "Shell",
    "YAML",
    "CSS"
  ],
  "high_risk_areas": [
    "`packages/app-shared/src/router` because one router drives three apps",
    "`packages/app-shared/src/menu.config.ts` because it is the shared navigation contract",
    "`apps/*/viewRegistry.ts` and `apps/*/vite.config.ts` because they swap app-specific pages via aliases",
    "`backend/aep_backend/settings.py` middleware and app registration",
    "`backend/core/urls/api.py` because it concentrates a very large API surface",
    "`backend/core/middleware/rls.py` and related auth/tenant middleware",
    "`gcp-run-proxy/src/worker.mjs` because edge security behavior lives there"
  ],
  "navigation_order": [
    "`README.md`",
    "`docs/architecture/architecture-summary.md`",
    "`package.json` and `pnpm-workspace.yaml`",
    "`apps/customer/src/main.tsx` and `packages/app-shared/src/app/Bootstrap.tsx`",
    "`packages/app-shared/src/router/index.tsx` and `packages/app-shared/src/menu.config.ts`",
    "`apps/mordor/viewRegistry.ts` and `apps/organizations/viewRegistry.ts`",
    "`backend/aep_backend/settings.py`",
    "`backend/aep_backend/urls.py` and `backend/core/urls/api.py`",
    "`gcp-run-proxy/src/worker.mjs`",
    "`scripts/README.md`, `ai-agent-onboarding.md`, and `e2e/README.md`"
  ],
  "representative_code_files": [
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/customer/src/main.tsx",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/customer/src/entry-authenticated.tsx",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/app/Bootstrap.tsx",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/router/index.tsx",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/router/BuildRouter.tsx",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/menu.config.ts",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/mordor/viewRegistry.ts",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/organizations/viewRegistry.ts",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/aep_backend/settings.py",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/aep_backend/urls.py",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/aep_backend/asgi.py",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/gcp-run-proxy/src/worker.mjs"
  ],
  "representative_docs": [
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/README.md",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/architecture/architecture-summary.md",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/architecture/structure.md",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/README.md",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/ai-agent-onboarding.md",
    "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/e2e/README.md"
  ],
  "evidence": [
    "`README.md` describes Aeptus as a config-driven enterprise GRC platform with dual SPAs and points to `pnpm order66` as the one-command local start flow.",
    "`pnpm-workspace.yaml` shows a real monorepo: `apps/*`, `packages/*`, `tools/*`, and `gcp-run-proxy`.",
    "`apps/*/src/main.tsx` use a shared pre-auth module before loading the authenticated React app, so the app folders are intentionally thin.",
    "`packages/app-shared/src/router/index.tsx` builds router configs for three deployment modes: tenant customer routes, `/o/:orgSlug` org routes, and flat Mordor staff routes.",
    "`packages/app-shared/src/menu.config.ts` is the single typed route/navigation source for the shared product surface.",
    "`apps/mordor/viewRegistry.ts` and `apps/organizations/viewRegistry.ts` show the extension model: reuse the base registry, then add app-specific pages.",
    "`backend/aep_backend/settings.py` registers many Django apps and a deep middleware stack for CORS, CSP, auth, API keys, impersonation, RLS, metrics, request logging, audit, and usage tracking.",
    "`backend/aep_backend/urls.py` and `backend/core/urls/api.py` show a very broad API surface spanning auth, admin tooling, directory/ADN, third parties, controls, information, events, webhooks, and more.",
    "`backend/aep_backend/asgi.py` wires HTTP plus optional Channels websocket routes for collaboration/onboarding/accounts.",
    "`gcp-run-proxy/src/worker.mjs` implements edge CORS, CSP/security headers, cookie hardening, and websocket proxying, so Cloudflare is an active security layer rather than static hosting only.",
    "`scripts/README.md`, `ai-agent-onboarding.md`, and `Agents/skills/*` show the repo is optimized for automation and AI-assisted development, not just manual workflows.",
    "`e2e/README.md` shows substantial Playwright coverage and a page-object/API-mocking testing approach."
  ]
}
```

### Raw Run Record

```json
{
  "structured_output": {
    "repo_summary": "Aeptus is a large monorepo for a machine-first GRC platform: three React SPAs on the frontend, a multi-tenant Django backend, a Cloudflare Worker edge proxy, and substantial documentation/agent/tooling around all of it. The dominant pattern is shared product logic in `packages/app-shared`, with the app folders acting mostly as thin shells for build target, route scope, and per-app view registry.",
    "code_areas": [
      "Frontend apps in `apps/customer`, `apps/mordor`, and `apps/organizations`",
      "Shared frontend product layer in `packages/app-shared/src`",
      "Design system in `packages/ui/src`",
      "Frontend auth/config/types packages in `packages/auth`, `packages/config`, `packages/types`",
      "Django backend in `backend/*` with many domain apps",
      "Cloudflare Worker proxy in `gcp-run-proxy/src/worker.mjs`",
      "Repo automation in `scripts/*` and AI/agent workflow in `Agents/*`"
    ],
    "reference_areas": [
      "`README.md`",
      "`docs/architecture/architecture-summary.md`",
      "`docs/architecture/structure.md`",
      "`Agents/skills/product-vision/skill.md`",
      "`backend/README.md`",
      "`ai-agent-onboarding.md`",
      "`e2e/README.md`"
    ],
    "entrypoints": [
      "`apps/customer/src/main.tsx`",
      "`apps/customer/src/entry-authenticated.tsx`",
      "`apps/mordor/src/main.tsx`",
      "`apps/organizations/src/main.tsx`",
      "`packages/app-shared/src/app/Bootstrap.tsx`",
      "`packages/app-shared/src/router/index.tsx`",
      "`backend/manage.py`",
      "`backend/aep_backend/asgi.py`",
      "`backend/aep_backend/urls.py`",
      "`gcp-run-proxy/src/worker.mjs`"
    ],
    "important_docs": [
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/README.md`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/architecture/architecture-summary.md`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/architecture/structure.md`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/README.md`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/ai-agent-onboarding.md`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/e2e/README.md`"
    ],
    "key_configs": [
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/package.json`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/pnpm-workspace.yaml`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/vite.config.mjs`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/customer/vite.config.ts`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/mordor/vite.config.ts`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/organizations/vite.config.ts`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/aep_backend/settings.py`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/wrangler.toml`"
    ],
    "key_languages": [
      "TypeScript",
      "React",
      "Python",
      "Shell",
      "YAML",
      "CSS"
    ],
    "high_risk_areas": [
      "`packages/app-shared/src/router` because one router drives three apps",
      "`packages/app-shared/src/menu.config.ts` because it is the shared navigation contract",
      "`apps/*/viewRegistry.ts` and `apps/*/vite.config.ts` because they swap app-specific pages via aliases",
      "`backend/aep_backend/settings.py` middleware and app registration",
      "`backend/core/urls/api.py` because it concentrates a very large API surface",
      "`backend/core/middleware/rls.py` and related auth/tenant middleware",
      "`gcp-run-proxy/src/worker.mjs` because edge security behavior lives there"
    ],
    "navigation_order": [
      "`README.md`",
      "`docs/architecture/architecture-summary.md`",
      "`package.json` and `pnpm-workspace.yaml`",
      "`apps/customer/src/main.tsx` and `packages/app-shared/src/app/Bootstrap.tsx`",
      "`packages/app-shared/src/router/index.tsx` and `packages/app-shared/src/menu.config.ts`",
      "`apps/mordor/viewRegistry.ts` and `apps/organizations/viewRegistry.ts`",
      "`backend/aep_backend/settings.py`",
      "`backend/aep_backend/urls.py` and `backend/core/urls/api.py`",
      "`gcp-run-proxy/src/worker.mjs`",
      "`scripts/README.md`, `ai-agent-onboarding.md`, and `e2e/README.md`"
    ],
    "representative_code_files": [
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/customer/src/main.tsx",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/customer/src/entry-authenticated.tsx",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/app/Bootstrap.tsx",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/router/index.tsx",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/router/BuildRouter.tsx",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/menu.config.ts",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/mordor/viewRegistry.ts",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/organizations/viewRegistry.ts",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/aep_backend/settings.py",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/aep_backend/urls.py",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/aep_backend/asgi.py",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/gcp-run-proxy/src/worker.mjs"
    ],
    "representative_docs": [
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/README.md",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/architecture/architecture-summary.md",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/architecture/structure.md",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/README.md",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/ai-agent-onboarding.md",
      "/Users/christophehenner/Downloads/Repositories/Playground Aethyme/e2e/README.md"
    ],
    "evidence": [
      "`README.md` describes Aeptus as a config-driven enterprise GRC platform with dual SPAs and points to `pnpm order66` as the one-command local start flow.",
      "`pnpm-workspace.yaml` shows a real monorepo: `apps/*`, `packages/*`, `tools/*`, and `gcp-run-proxy`.",
      "`apps/*/src/main.tsx` use a shared pre-auth module before loading the authenticated React app, so the app folders are intentionally thin.",
      "`packages/app-shared/src/router/index.tsx` builds router configs for three deployment modes: tenant customer routes, `/o/:orgSlug` org routes, and flat Mordor staff routes.",
      "`packages/app-shared/src/menu.config.ts` is the single typed route/navigation source for the shared product surface.",
      "`apps/mordor/viewRegistry.ts` and `apps/organizations/viewRegistry.ts` show the extension model: reuse the base registry, then add app-specific pages.",
      "`backend/aep_backend/settings.py` registers many Django apps and a deep middleware stack for CORS, CSP, auth, API keys, impersonation, RLS, metrics, request logging, audit, and usage tracking.",
      "`backend/aep_backend/urls.py` and `backend/core/urls/api.py` show a very broad API surface spanning auth, admin tooling, directory/ADN, third parties, controls, information, events, webhooks, and more.",
      "`backend/aep_backend/asgi.py` wires HTTP plus optional Channels websocket routes for collaboration/onboarding/accounts.",
      "`gcp-run-proxy/src/worker.mjs` implements edge CORS, CSP/security headers, cookie hardening, and websocket proxying, so Cloudflare is an active security layer rather than static hosting only.",
      "`scripts/README.md`, `ai-agent-onboarding.md`, and `Agents/skills/*` show the repo is optimized for automation and AI-assisted development, not just manual workflows.",
      "`e2e/README.md` shows substantial Playwright coverage and a page-object/API-mocking testing approach."
    ]
  },
  "input_tokens": 153431,
  "output_tokens": 0,
  "command": "codex exec (via Chau7 tab)",
  "exit_code": 0,
  "retries": 0,
  "review_burden": 0,
  "duration_seconds": 0,
  "final_output_message": "Aeptus is a large monorepo for a machine-first GRC platform: three React SPAs on the frontend, a multi-tenant Django backend, a Cloudflare Worker edge proxy, and substantial documentation/agent/tooling around all of it. The dominant pattern is shared product logic in `packages/app-shared`, with the app folders acting mostly as thin shells for build target, route scope, and per-app view registry.",
  "stderr_size": 72760
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
      "absolute_count": 18,
      "markdown_link_count": 0,
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
Task: Explain this repo
Repository path: /Users/christophehenner/Downloads/Repositories/Playground Aethyme
Use Aethyme tools to navigate the repository graph. Explore the repository and produce a structured explanation.
```

### Run Metrics

- command: `codex exec (via Chau7 tab)`
- exit code: `0`
- input tokens: `169911`
- output tokens: `0`
- retries: `0`
- review burden: `0`
- wall time: `0.000s`

### Final Output Message

```text
Aeptus is a large monorepo for a multi-tenant GRC platform. The core shape is three React/Vite SPAs on the frontend, a large Django backend on the server side, shared packages for UI/auth/config/application logic, and a Cloudflare Worker in front of the API. The dominant architectural idea is that app-specific shells stay thin while routing, feature composition, and shared product behavior live in `packages/app-shared`, and backend domains are split into many Django apps with hard tenant isolation and bitemporal/audit-oriented data patterns.
```

### Structured Output

```json
{
  "repo_summary": "Aeptus is a large monorepo for a multi-tenant GRC platform. The core shape is three React/Vite SPAs on the frontend, a large Django backend on the server side, shared packages for UI/auth/config/application logic, and a Cloudflare Worker in front of the API. The dominant architectural idea is that app-specific shells stay thin while routing, feature composition, and shared product behavior live in `packages/app-shared`, and backend domains are split into many Django apps with hard tenant isolation and bitemporal/audit-oriented data patterns.",
  "code_areas": [
    "Frontend app shells in `apps/customer`, `apps/mordor`, and `apps/organizations`",
    "Shared frontend product layer in `packages/app-shared`",
    "Shared design system in `packages/ui`",
    "Shared auth/config packages in `packages/auth` and `packages/config`",
    "Django backend domains under `backend/*`",
    "Cloudflare API proxy in `gcp-run-proxy`",
    "Operational tooling and repo workflows in `scripts`, `tools`, `docker`, and `devops`"
  ],
  "reference_areas": [
    "`README.md` for repo-level workflow and architecture entry",
    "`docs/architecture/architecture-summary.md` for the top-level system model",
    "`backend/README.md` for backend domain/data overview",
    "`pnpm-workspace.yaml` for monorepo structure"
  ],
  "entrypoints": [
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/customer/src/main.tsx`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/mordor/src/main.tsx`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/organizations/src/main.tsx`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/app/Bootstrap.tsx`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/router/index.tsx`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/manage.py`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/aep_backend/asgi.py`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/aep_backend/urls.py`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/gcp-run-proxy/src/worker.mjs`"
  ],
  "important_docs": [
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/README.md`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/architecture/architecture-summary.md`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/README.md`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/AGENTS.md`"
  ],
  "key_configs": [
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/pnpm-workspace.yaml`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/package.json`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/vite.config.mjs`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/aep_backend/settings.py`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docker-compose.yml`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/wrangler.toml`"
  ],
  "key_languages": [
    "TypeScript",
    "React",
    "Python",
    "Django",
    "JavaScript"
  ],
  "high_risk_areas": [
    "Shared routing/menu/config coupling in `packages/app-shared`",
    "Backend tenant isolation and request context in `backend/core/middleware/rls.py`",
    "Large backend composition surface in `backend/aep_backend/settings.py` and `backend/aep_backend/urls.py`",
    "Cloudflare-to-Django security boundary in `gcp-run-proxy/src/worker.mjs`",
    "Existing circular dependencies reported by dependency graph tooling around config/menu/error-reporting paths"
  ],
  "navigation_order": [
    "Start with `README.md`",
    "Then read `docs/architecture/architecture-summary.md`",
    "For frontend architecture, read `packages/app-shared/src/app/Bootstrap.tsx`, then `packages/app-shared/src/router/index.tsx`, then `packages/app-shared/src/menu.config.ts`",
    "Then inspect one app shell: `apps/customer`, `apps/mordor`, or `apps/organizations` via each app\u2019s `main.tsx`, `entry-authenticated.tsx`, `menu.config.ts`, and `vite.config.ts`",
    "For backend, read `backend/README.md`, then `backend/aep_backend/settings.py`, `backend/aep_backend/urls.py`, `backend/core/middleware/rls.py`, and `backend/core/temporal.py`",
    "Finish with infrastructure and tooling: `gcp-run-proxy/src/worker.mjs`, `docker-compose.yml`, and root `scripts/`"
  ],
  "representative_code_files": [
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/customer/src/main.tsx`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/customer/src/entry-authenticated.tsx`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/mordor/viewRegistry.ts`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/mordor/menu.config.ts`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/app/Bootstrap.tsx`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/router/index.tsx`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/router/BuildRouter.tsx`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/nav.ts`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/ui/src/index.ts`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/aep_backend/settings.py`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/aep_backend/urls.py`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/core/middleware/rls.py`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/core/temporal.py`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/gcp-run-proxy/src/worker.mjs`"
  ],
  "representative_docs": [
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/README.md`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/architecture/architecture-summary.md`",
    "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/README.md`"
  ],
  "evidence": [
    "The Aethyme skill\u2019s prescribed command failed in this checkout because `.venv/bin/python -m src.cli ...` returned `No module named src.cli`, so the repo does not currently expose that graph CLI at the advertised entrypoint.",
    "`pnpm-workspace.yaml` defines a monorepo with `apps/*`, `packages/*`, `tools/*`, and `gcp-run-proxy`.",
    "Each SPA starts at a tiny pre-auth entry in `apps/*/src/main.tsx` and then loads `entry-authenticated.tsx`.",
    "`packages/app-shared/src/app/Bootstrap.tsx` is the shared frontend bootstrap and `packages/app-shared/src/router/index.tsx` is the shared router.",
    "`packages/app-shared/src/menu.config.ts` plus per-app `menu.config.ts` files show that navigation is data-driven and app-specific differences are mostly configuration and view-registry changes.",
    "`backend/aep_backend/settings.py` shows a large Django composition root with many domain apps and middleware enforcing auth, RBAC, audit, and tenant RLS.",
    "`backend/aep_backend/asgi.py` and `backend/aep_backend/celery.py` show optional realtime support and async task discovery.",
    "`gcp-run-proxy/src/worker.mjs` shows Cloudflare handling CORS, CSP, cookie hardening, and proxying to the backend.",
    "A dependency-cruiser pass over `apps` and `packages` cruised 2359 modules / 7206 dependencies and confirmed that most coupling is in shared packages; it also reported circular dependencies around `packages/app-shared` config/menu/error-reporting paths."
  ]
}
```

### Raw Run Record

```json
{
  "structured_output": {
    "repo_summary": "Aeptus is a large monorepo for a multi-tenant GRC platform. The core shape is three React/Vite SPAs on the frontend, a large Django backend on the server side, shared packages for UI/auth/config/application logic, and a Cloudflare Worker in front of the API. The dominant architectural idea is that app-specific shells stay thin while routing, feature composition, and shared product behavior live in `packages/app-shared`, and backend domains are split into many Django apps with hard tenant isolation and bitemporal/audit-oriented data patterns.",
    "code_areas": [
      "Frontend app shells in `apps/customer`, `apps/mordor`, and `apps/organizations`",
      "Shared frontend product layer in `packages/app-shared`",
      "Shared design system in `packages/ui`",
      "Shared auth/config packages in `packages/auth` and `packages/config`",
      "Django backend domains under `backend/*`",
      "Cloudflare API proxy in `gcp-run-proxy`",
      "Operational tooling and repo workflows in `scripts`, `tools`, `docker`, and `devops`"
    ],
    "reference_areas": [
      "`README.md` for repo-level workflow and architecture entry",
      "`docs/architecture/architecture-summary.md` for the top-level system model",
      "`backend/README.md` for backend domain/data overview",
      "`pnpm-workspace.yaml` for monorepo structure"
    ],
    "entrypoints": [
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/customer/src/main.tsx`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/mordor/src/main.tsx`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/organizations/src/main.tsx`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/app/Bootstrap.tsx`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/router/index.tsx`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/manage.py`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/aep_backend/asgi.py`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/aep_backend/urls.py`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/gcp-run-proxy/src/worker.mjs`"
    ],
    "important_docs": [
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/README.md`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/architecture/architecture-summary.md`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/README.md`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/AGENTS.md`"
    ],
    "key_configs": [
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/pnpm-workspace.yaml`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/package.json`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/vite.config.mjs`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/aep_backend/settings.py`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docker-compose.yml`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/wrangler.toml`"
    ],
    "key_languages": [
      "TypeScript",
      "React",
      "Python",
      "Django",
      "JavaScript"
    ],
    "high_risk_areas": [
      "Shared routing/menu/config coupling in `packages/app-shared`",
      "Backend tenant isolation and request context in `backend/core/middleware/rls.py`",
      "Large backend composition surface in `backend/aep_backend/settings.py` and `backend/aep_backend/urls.py`",
      "Cloudflare-to-Django security boundary in `gcp-run-proxy/src/worker.mjs`",
      "Existing circular dependencies reported by dependency graph tooling around config/menu/error-reporting paths"
    ],
    "navigation_order": [
      "Start with `README.md`",
      "Then read `docs/architecture/architecture-summary.md`",
      "For frontend architecture, read `packages/app-shared/src/app/Bootstrap.tsx`, then `packages/app-shared/src/router/index.tsx`, then `packages/app-shared/src/menu.config.ts`",
      "Then inspect one app shell: `apps/customer`, `apps/mordor`, or `apps/organizations` via each app\u2019s `main.tsx`, `entry-authenticated.tsx`, `menu.config.ts`, and `vite.config.ts`",
      "For backend, read `backend/README.md`, then `backend/aep_backend/settings.py`, `backend/aep_backend/urls.py`, `backend/core/middleware/rls.py`, and `backend/core/temporal.py`",
      "Finish with infrastructure and tooling: `gcp-run-proxy/src/worker.mjs`, `docker-compose.yml`, and root `scripts/`"
    ],
    "representative_code_files": [
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/customer/src/main.tsx`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/customer/src/entry-authenticated.tsx`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/mordor/viewRegistry.ts`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/apps/mordor/menu.config.ts`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/app/Bootstrap.tsx`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/router/index.tsx`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/router/BuildRouter.tsx`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/app-shared/src/nav.ts`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/packages/ui/src/index.ts`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/aep_backend/settings.py`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/aep_backend/urls.py`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/core/middleware/rls.py`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/core/temporal.py`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/gcp-run-proxy/src/worker.mjs`"
    ],
    "representative_docs": [
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/README.md`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/docs/architecture/architecture-summary.md`",
      "`/Users/christophehenner/Downloads/Repositories/Playground Aethyme/backend/README.md`"
    ],
    "evidence": [
      "The Aethyme skill\u2019s prescribed command failed in this checkout because `.venv/bin/python -m src.cli ...` returned `No module named src.cli`, so the repo does not currently expose that graph CLI at the advertised entrypoint.",
      "`pnpm-workspace.yaml` defines a monorepo with `apps/*`, `packages/*`, `tools/*`, and `gcp-run-proxy`.",
      "Each SPA starts at a tiny pre-auth entry in `apps/*/src/main.tsx` and then loads `entry-authenticated.tsx`.",
      "`packages/app-shared/src/app/Bootstrap.tsx` is the shared frontend bootstrap and `packages/app-shared/src/router/index.tsx` is the shared router.",
      "`packages/app-shared/src/menu.config.ts` plus per-app `menu.config.ts` files show that navigation is data-driven and app-specific differences are mostly configuration and view-registry changes.",
      "`backend/aep_backend/settings.py` shows a large Django composition root with many domain apps and middleware enforcing auth, RBAC, audit, and tenant RLS.",
      "`backend/aep_backend/asgi.py` and `backend/aep_backend/celery.py` show optional realtime support and async task discovery.",
      "`gcp-run-proxy/src/worker.mjs` shows Cloudflare handling CORS, CSP, cookie hardening, and proxying to the backend.",
      "A dependency-cruiser pass over `apps` and `packages` cruised 2359 modules / 7206 dependencies and confirmed that most coupling is in shared packages; it also reported circular dependencies around `packages/app-shared` config/menu/error-reporting paths."
    ]
  },
  "input_tokens": 169911,
  "output_tokens": 0,
  "command": "codex exec (via Chau7 tab)",
  "exit_code": 0,
  "retries": 0,
  "review_burden": 0,
  "duration_seconds": 0,
  "final_output_message": "Aeptus is a large monorepo for a multi-tenant GRC platform. The core shape is three React/Vite SPAs on the frontend, a large Django backend on the server side, shared packages for UI/auth/config/application logic, and a Cloudflare Worker in front of the API. The dominant architectural idea is that app-specific shells stay thin while routing, feature composition, and shared product behavior live in `packages/app-shared`, and backend domains are split into many Django apps with hard tenant isolation and bitemporal/audit-oriented data patterns.",
  "stderr_size": 91926
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
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 5.0,
    "normalized_score": 5.0,
    "normalization_delta": 0.0
  }
}
```


## Context Pack Audit

### Pack Summary

- Anchors: `5`
- Navigation order items: `5`
- In-scope files: `0`
- CLI commands: `3`

### Navigation Context

```json
{
  "mode": "iterative_navigation",
  "repo_path": "/Users/christophehenner/Downloads/Repositories/Playground Aethyme",
  "tool_repo_path": "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme",
  "tool_python": "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python",
  "task": "Explain this repo",
  "anchors": {
    "task": "Explain this repo",
    "anchors": [
      {
        "kind": "file",
        "id": "tools/mcp-mordor/README.md",
        "file": "tools/mcp-mordor/README.md",
        "reason": "repository readme"
      },
      {
        "kind": "file",
        "id": "docs/adr/010-monorepo-architecture.md",
        "file": "docs/adr/010-monorepo-architecture.md",
        "reason": "architecture document"
      },
      {
        "kind": "folder",
        "id": "packages",
        "file": null,
        "reason": "top-level area"
      },
      {
        "kind": "folder",
        "id": "tools",
        "file": null,
        "reason": "top-level area"
      },
      {
        "kind": "file",
        "id": "Agents/skills/_meta/scripts/add_frontmatter.py",
        "file": "Agents/skills/_meta/scripts/add_frontmatter.py",
        "reason": "likely entrypoint"
      }
    ]
  },
  "scope": {
    "task": "Explain this repo",
    "navigation_order": [
      "tools/mcp-mordor/README.md",
      "docs/adr/010-monorepo-architecture.md",
      "packages",
      "tools",
      "Agents/skills/_meta/scripts/add_frontmatter.py"
    ],
    "in_scope_files": [
      "tools/mcp-mordor/README.md"
    ],
    "in_scope_symbols": [],
    "in_scope_areas": [
      "packages",
      "tools"
    ],
    "out_of_scope": [],
    "risks": []
  },
  "commands": [
    "cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli repo inspect '/Users/christophehenner/Downloads/Repositories/Playground Aethyme' --json-output",
    "cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph overview '/Users/christophehenner/Downloads/Repositories/Playground Aethyme' --json-output",
    "cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph expand '/Users/christophehenner/Downloads/Repositories/Playground Aethyme' <anchor-id> --json-output"
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

| Metric | Control (CTO off) | Control (CTO on) | Explore | Leverage |
| --- | --- | --- | --- | --- |
| Prompt chars | `167` | `167` | `167` | `219` |
| Wall time | `0.0s` | `0.0s` | `0.0s` | `0.0s` |
| Input tokens | `30447` | `121525` | `153431` | `169911` |
| Output tokens | `0` | `0` | `0` | `0` |
| Score | `10.0` | `5.0` | `5.0` | `5.0` |

- Navigation items surfaced: `5`
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
    "Repo summary is informative but not currently machine-scored.",
    "Path normalization strips markdown links, line anchors, absolute prefixes, and leading ./ before comparison."
  ]
}
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

## Aethyme Pack

```json
{
  "task": {
    "raw": "Explain this repo",
    "normalized": "explain this repo",
    "kind": "explain_repo"
  },
  "summary": {
    "snapshot": {
      "languages": [
        "javascript",
        "python",
        "typescript"
      ],
      "top_level_dirs": [
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
    "files_count": 106111,
    "functions_count": 12818,
    "classes_count": 3271,
    "docs_count": 1073,
    "configs_count": 79
  },
  "signals": {
    "boundary_clarity": {
      "score": 68,
      "level": "mixed",
      "evidence": [
        "cross-area semantic edges: 41610/264763",
        "source files with area assignment: 5401/5420",
        "generic source file names: 13"
      ]
    },
    "entrypoint_clarity": {
      "score": 100,
      "level": "strong",
      "evidence": [
        "direct code entrypoint edges: 1197",
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
        "low-confidence semantic edges: 209685/236039",
        "high-confidence semantic edges: 14717/236039",
        "cross-area semantic edges: 31018/236039"
      ]
    },
    "parser_visibility": {
      "score": 87,
      "level": "strong",
      "evidence": [
        "supported source files: 5130/5420",
        "source files with semantic extraction: 3828/5420",
        "total extracted functions/classes: 16089"
      ]
    }
  },
  "overview": {
    "overview_docs": [
      "Agents/Skills Manager/README.md",
      "Agents/skills/README.md",
      "Agents/skills/architecture/SKILL.md"
    ],
    "code_areas": [
      "backend",
      "packages",
      "scripts"
    ],
    "reference_areas": [
      "docs",
      "test-results"
    ],
    "subareas": [
      "backend/accounts",
      "backend/core",
      "packages/app-shared",
      "packages/ui"
    ],
    "entrypoints": [
      "packages/auth/src/index.ts",
      "packages/config/src/index.ts",
      "packages/types/src/index.ts"
    ],
    "key_configs": [
      "backend/pyproject.toml",
      "packages/auth/package.json"
    ],
    "representative_code_files": [
      "Agents/skills/_meta/scripts/add_frontmatter.py",
      "Agents/skills/_meta/scripts/analyze_repo.py",
      "Agents/skills/_meta/scripts/analyze_usage_logs.py",
      "Agents/skills/_meta/scripts/build_learning_index.py",
      "Agents/skills/_meta/scripts/build_skills_registry.py"
    ],
    "representative_docs": [
      "Agents/Skills Manager/README.md",
      "Agents/skills/README.md",
      "Agents/skills/architecture/SKILL.md",
      "Agents/skills/architecture/references/adr-index.md",
      "Agents/skills/architecture/references/data-flow.md"
    ]
  },
  "anchors": [
    {
      "kind": "file",
      "id": "tools/mcp-mordor/README.md",
      "file": "tools/mcp-mordor/README.md",
      "reason": "repository readme"
    },
    {
      "kind": "file",
      "id": "docs/adr/010-monorepo-architecture.md",
      "file": "docs/adr/010-monorepo-architecture.md",
      "reason": "architecture document"
    },
    {
      "kind": "folder",
      "id": "packages",
      "file": null,
      "reason": "top-level area"
    },
    {
      "kind": "folder",
      "id": "tools",
      "file": null,
      "reason": "top-level area"
    },
    {
      "kind": "file",
      "id": "Agents/skills/_meta/scripts/add_frontmatter.py",
      "file": "Agents/skills/_meta/scripts/add_frontmatter.py",
      "reason": "likely entrypoint"
    }
  ],
  "in_scope": {
    "files": [
      {
        "value": "tools/mcp-mordor/README.md",
        "kind": "file",
        "reason": "anchor-adjacent file"
      }
    ],
    "symbols": [],
    "areas": [
      {
        "value": "packages",
        "kind": "area",
        "reason": "primary top-level area"
      },
      {
        "value": "tools",
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
      "from": "Agents/skills/_meta/scripts/add_frontmatter.py",
      "to": "Agents/skills/_meta/scripts/add_frontmatter.py::generate_frontmatter",
      "kind": "defines"
    },
    {
      "from": "Agents/skills/_meta/scripts/add_frontmatter.py",
      "to": "Agents/skills/_meta/scripts/add_frontmatter.py::has_frontmatter",
      "kind": "defines"
    },
    {
      "from": "Agents/skills/_meta/scripts/add_frontmatter.py",
      "to": "Agents/skills/_meta/scripts/add_frontmatter.py::main",
      "kind": "defines"
    },
    {
      "from": "Agents/skills/_meta/scripts/add_frontmatter.py",
      "to": "Agents/skills/_meta/scripts/add_frontmatter.py::process_file",
      "kind": "defines"
    },
    {
      "from": "Agents/skills/_meta/scripts/analyze_repo.py",
      "to": "Agents/skills/_meta/scripts/analyze_repo.py::RepoAnalyzer",
      "kind": "defines"
    },
    {
      "from": "Agents/skills/_meta/scripts/analyze_repo.py",
      "to": "Agents/skills/_meta/scripts/analyze_repo.py::__init__",
      "kind": "defines"
    }
  ],
  "impact": [
    {
      "symbol": "packages/auth/src/index.ts",
      "file": "packages/auth/src/index.ts",
      "reason": "entrypoint candidate"
    },
    {
      "symbol": "packages/config/src/index.ts",
      "file": "packages/config/src/index.ts",
      "reason": "entrypoint candidate"
    }
  ],
  "snippets": [
    {
      "file": "Agents/skills/_meta/scripts/add_frontmatter.py",
      "start_line": 1,
      "end_line": 20,
      "kind": "overview"
    },
    {
      "file": "docs/adr/010-monorepo-architecture.md",
      "start_line": 1,
      "end_line": 20,
      "kind": "overview"
    },
    {
      "file": "tools/mcp-mordor/README.md",
      "start_line": 1,
      "end_line": 20,
      "kind": "overview"
    }
  ],
  "file_contents": [],
  "risk_flags": [],
  "navigation_order": [
    "tools/mcp-mordor/README.md",
    "docs/adr/010-monorepo-architecture.md",
    "packages",
    "tools",
    "Agents/skills/_meta/scripts/add_frontmatter.py"
  ],
  "budget": {
    "max_anchors": 5,
    "max_files": 8,
    "max_snippets": 8,
    "dependency_depth": 1,
    "impact_depth": 1,
    "snippet_window": 20,
    "content_budget": 80000,
    "max_content_files": 15,
    "max_lines_per_file": 500
  },
  "confidence": {
    "anchor_confidence": 0.85,
    "scope_confidence": 0.8
  },
  "activation_summary": {
    "activated_node_count": 120678,
    "max_depth_reached": 3,
    "top_activated": [
      {
        "id": "area:Playground Aethyme:packages",
        "activation": 1.0
      },
      {
        "id": "area:Playground Aethyme:tools",
        "activation": 1.0
      },
      {
        "id": "file:Playground Aethyme:Agents/skills/_meta/scripts/add_frontmatter.py",
        "activation": 1.0
      },
      {
        "id": "doc:Playground Aethyme:tools/mcp-mordor/README.md",
        "activation": 1.0
      },
      {
        "id": "file:Playground Aethyme:docs/adr/010-monorepo-architecture.md",
        "activation": 1.0
      },
      {
        "id": "doc:Playground Aethyme:docs/adr/010-monorepo-architecture.md",
        "activation": 1.0
      },
      {
        "id": "dir:Playground Aethyme:packages",
        "activation": 1.0
      },
      {
        "id": "dir:Playground Aethyme:tools/mcp-mordor/src/tools",
        "activation": 1.0
      },
      {
        "id": "dir:Playground Aethyme:tools",
        "activation": 1.0
      },
      {
        "id": "dir:Playground Aethyme:scripts/tools",
        "activation": 1.0
      }
    ]
  }
}
```

## Explanation

```text
Task: Explain this repo
Languages: javascript, python, typescript
Top-level directories: .gcloud_tmp, .githooks, .github, .husky, .hypothesis, .lighthouseci, .playwright-mcp, .pnpm-store, .storybook, .wrangler, Agents, TODO, alerts, apps, backend, catalog, config, contracts, devops, docker, docs, e2e, functions, gcp-run-proxy, grafana-provisioning, load_tests, logs, output, packages, patches, playwright-report, project, public, scripts, shared, src, stories, test-results, tests, tools
Files indexed: 106111
Functions indexed: 12818
Classes indexed: 3271
Docs indexed: 1073
Configs indexed: 79
README: Agents/Skills Manager/README.md

Code areas:
- backend
- packages
- scripts

Reference areas:
- docs
- test-results

Key subareas:
- backend/accounts
- backend/core
- packages/app-shared
- packages/ui

Key configs:
- backend/pyproject.toml
- packages/auth/package.json

Entrypoints:
- packages/auth/src/index.ts
- packages/config/src/index.ts
- packages/types/src/index.ts

Representative code:
- Agents/skills/_meta/scripts/add_frontmatter.py
- Agents/skills/_meta/scripts/analyze_repo.py
- Agents/skills/_meta/scripts/analyze_usage_logs.py

Representative docs:
- Agents/Skills Manager/README.md
- Agents/skills/README.md
- Agents/skills/architecture/SKILL.md

Navigation order:
- tools/mcp-mordor/README.md
- docs/adr/010-monorepo-architecture.md
- packages
- tools
- Agents/skills/_meta/scripts/add_frontmatter.py
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
