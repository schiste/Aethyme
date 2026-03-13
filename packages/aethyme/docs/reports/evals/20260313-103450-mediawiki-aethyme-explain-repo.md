# Eval Report: explain-repo (MediaWiki, reasoning=low)

Last Updated: 2026-03-13

- Repository: `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme`
- Generated: `2026-03-13T10:34:50.226242+00:00`
- Conditions: `control-cto-off, control-cto-on, explore, leverage`

## Summary

- Control (CTO off) prompt chars: `189`
- Control (CTO on) prompt chars: `189`
- Explore prompt chars: `189`
- Leverage prompt chars: `241`
- Navigation items: `0`
- Risk items: `0`

### Scorecard

| Condition | Score | Tokens | Tool Calls |
| --- | --- | --- | --- |
| Control (CTO off) | 0.0 | 181K | - |
| Control (CTO on) | 5.67 | 200K | - |
| Explore | 0.67 | 182K | - |
| Leverage | 5.67 | 117K | - |

## Control (CTO off)

### Prompt

```text
Task: Explain this repo
Repository path: /Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control
Explore the repository and produce a structured explanation.
```

### Run Metrics

- command: `codex exec -s danger-full-access -c 'reasoning_effort="low"' --output-schema /tmp/aethyme-eval-output-schema.json -o /tmp/aethyme-eval-mw-low-control-cto-off-result.json - < /tmp/aethyme-eval-control-cto-off-prompt.txt`
- exit code: `0`
- input tokens: `181434`
- output tokens: `0`
- retries: `0`
- review burden: `0`
- wall time: `0.000s`

### Final Output Message

```text
tokens used
181434
```

### Structured Output

```json
{
  "repo_summary": "This repository is MediaWiki core, not an app built on top of MediaWiki. It is a large PHP 8.2+ wiki platform with its own bootstrap/config pipeline, dependency-injection container, extension/skin registration system, frontend asset loader, installer, maintenance CLI, database schema, localization data, and a broad multi-layer test suite. A good mental model is: thin entrypoints -> shared bootstrap -> settings and extension registration -> MediaWikiServices service container -> domain modules for pages, revisions, parsing, APIs, jobs, storage, permissions, and UI.",
  "code_areas": [
    "Web entrypoints and bootstrap in root PHP files plus `includes/WebStart.php` and `includes/Setup.php`",
    "Core service container and DI wiring in `includes/MediaWikiServices.php` and `includes/ServiceWiring.php`",
    "Domain modules under `includes/` such as `Api`, `Rest`, `Parser`, `Page`, `Revision`, `Storage`, `User`, `Permissions`, `Search`, `Session`, `JobQueue`, `SpecialPage`, `ResourceLoader`",
    "Frontend modules and assets in `resources/src`, vendor browser libraries in `resources/lib`, and module registration in `resources/Resources.php`",
    "Operational and admin scripts in `maintenance/`",
    "Installer UI in `mw-config/`",
    "Localization and language data in `languages/`",
    "Database schema and migrations in `sql/`",
    "Tests across PHPUnit, parser tests, QUnit, Jest, Selenium, and API integration under `tests/`"
  ],
  "reference_areas": [
    "`docs/Injection.md` for the intended DI architecture",
    "`docs/Hooks.md` for extension integration points",
    "`docs/database.md` for database and replication model",
    "`docs/contenthandler.md` and `docs/pageupdater.md` for page content and edit flow",
    "Top-level `README.md`, `INSTALL`, `UPGRADE`, `DEVELOPERS.md` for usage and contributor workflow"
  ],
  "entrypoints": [
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/index.php#L13` for browser/page requests",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/api.php` for the Action API",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/rest.php` for the REST API",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/load.php` for ResourceLoader frontend assets",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/mw-config/index.php` for the web installer/configurator",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/maintenance/run.php` for CLI maintenance scripts"
  ],
  "important_docs": [
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/README.md`",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/DEVELOPERS.md`",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/docs/Injection.md`",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/docs/Hooks.md`",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/docs/database.md`",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/docs/contenthandler.md`",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/docs/pageupdater.md`",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/maintenance/README`"
  ],
  "key_configs": [
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/composer.json` for PHP deps, scripts, and local dev commands",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/package.json` for JS test/tooling commands",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/Gruntfile.js` for lint/QUnit/Karma setup",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/tests/jest/jest.config.js` for focused Jest suites",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/docker-compose.yml` for local containerized development",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/includes/config-schema.php` and `docs/config-schema.yaml` for configuration schema",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/resources/Resources.php` for frontend module definitions",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/sql/tables.json` for canonical schema"
  ],
  "key_languages": [
    "PHP dominates the repository",
    "JavaScript is the main frontend/test language",
    "LESS and CSS are used for styling",
    "SQL defines schema and migrations",
    "JSON is heavily used for config, schemas, manifests, and localization data",
    "Vue single-file components exist for newer UI modules"
  ],
  "high_risk_areas": [
    "`includes/Setup.php` because it bootstraps the entire application and config lifecycle",
    "`includes/ServiceWiring.php` and `includes/MediaWikiServices.php` because they define global service construction and dependency boundaries",
    "`resources/Resources.php` and `includes/ResourceLoader/*` because frontend asset loading is custom and central to the UI",
    "`sql/` plus `includes/DB`, `includes/Page`, `includes/Revision`, `includes/Storage` because page/revision/storage behavior is tightly coupled to schema and replication assumptions",
    "`includes/Registration/*` because extension and skin loading is manifest-driven and affects startup behavior",
    "`includes/Parser/*` and content/revision flows because they drive rendering, edits, and cached derived data"
  ],
  "navigation_order": [
    "Read `README.md` and `DEVELOPERS.md` for context",
    "Inspect top-level entrypoints: `index.php`, `api.php`, `rest.php`, `load.php`, `maintenance/run.php`",
    "Read `includes/WebStart.php` and `includes/Setup.php` to understand startup",
    "Read `docs/Injection.md` and inspect `includes/MediaWikiServices.php` plus `includes/ServiceWiring.php`",
    "Map the domain area you care about under `includes/`",
    "For frontend work, inspect `resources/Resources.php` first, then `resources/src/...`",
    "For persistence/edit flow, read `docs/contenthandler.md`, `docs/pageupdater.md`, `docs/database.md`, then inspect `includes/Page`, `includes/Revision`, `includes/Storage`",
    "For extension points, read `docs/Hooks.md` and `includes/Registration/*`",
    "For execution and verification, inspect `composer.json`, `package.json`, and `tests/`"
  ],
  "representative_code_files": [
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/index.php`",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/includes/WebStart.php`",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/includes/Setup.php`",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/includes/MediaWikiServices.php`",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/includes/ServiceWiring.php`",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/includes/GlobalFunctions.php`",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/resources/Resources.php`",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/includes/Page/PageStore.php`",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/includes/Revision/RevisionStore.php`",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/mw-config/index.php`"
  ],
  "representative_docs": [
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/docs/Injection.md`",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/docs/Hooks.md`",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/docs/database.md`",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/docs/contenthandler.md`",
    "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/docs/pageupdater.md`"
  ],
  "evidence": [
    "`includes/Defines.php` defines `MW_VERSION` as `1.46.0-alpha`, so this is a current MediaWiki core checkout, not a small custom fork",
    "`composer.json` names the package `mediawiki/core` and requires PHP `>=8.2.0`",
    "Top-level web entrypoints are thin wrappers that all hand off to shared infrastructure via `includes/WebStart.php` and service-based entrypoint classes",
    "`includes/Setup.php` explicitly documents the full bootstrap lifecycle: env checks, config loading, extension loading, service container init, exception handling, and session init",
    "`docs/Injection.md` describes `MediaWikiServices` as the heart of dependency injection, and `includes/ServiceWiring.php` is the concrete service map",
    "`includes/GlobalFunctions.php` exposes `wfLoadExtension()` and `wfLoadSkin()`, confirming manifest-driven extensibility",
    "`extensions/README` and `skins/README` show that extensions and skins are expected to be dropped in separately; this checkout mainly contains core",
    "`resources/Resources.php` registers frontend modules, showing that MediaWiki uses its own ResourceLoader module system instead of a conventional SPA bundler",
    "`tests/` contains PHPUnit, parser, QUnit, Jest, Selenium, and API-testing trees, showing a broad verification strategy",
    "Directory counts show scale: about 3461 files in `includes/`, 2641 in `resources/`, 2665 in `tests/`, and 3154 in `languages/`"
  ]
}
```

### Raw Run Record

```json
{
  "command": "codex exec -s danger-full-access -c 'reasoning_effort=\"low\"' --output-schema /tmp/aethyme-eval-output-schema.json -o /tmp/aethyme-eval-mw-low-control-cto-off-result.json - < /tmp/aethyme-eval-control-cto-off-prompt.txt",
  "exit_code": 0,
  "input_tokens": 181434,
  "output_tokens": 0,
  "retries": 0,
  "review_burden": 0,
  "duration_seconds": 0.0,
  "final_output_message": "tokens used\n181434",
  "structured_output": {
    "repo_summary": "This repository is MediaWiki core, not an app built on top of MediaWiki. It is a large PHP 8.2+ wiki platform with its own bootstrap/config pipeline, dependency-injection container, extension/skin registration system, frontend asset loader, installer, maintenance CLI, database schema, localization data, and a broad multi-layer test suite. A good mental model is: thin entrypoints -> shared bootstrap -> settings and extension registration -> MediaWikiServices service container -> domain modules for pages, revisions, parsing, APIs, jobs, storage, permissions, and UI.",
    "code_areas": [
      "Web entrypoints and bootstrap in root PHP files plus `includes/WebStart.php` and `includes/Setup.php`",
      "Core service container and DI wiring in `includes/MediaWikiServices.php` and `includes/ServiceWiring.php`",
      "Domain modules under `includes/` such as `Api`, `Rest`, `Parser`, `Page`, `Revision`, `Storage`, `User`, `Permissions`, `Search`, `Session`, `JobQueue`, `SpecialPage`, `ResourceLoader`",
      "Frontend modules and assets in `resources/src`, vendor browser libraries in `resources/lib`, and module registration in `resources/Resources.php`",
      "Operational and admin scripts in `maintenance/`",
      "Installer UI in `mw-config/`",
      "Localization and language data in `languages/`",
      "Database schema and migrations in `sql/`",
      "Tests across PHPUnit, parser tests, QUnit, Jest, Selenium, and API integration under `tests/`"
    ],
    "reference_areas": [
      "`docs/Injection.md` for the intended DI architecture",
      "`docs/Hooks.md` for extension integration points",
      "`docs/database.md` for database and replication model",
      "`docs/contenthandler.md` and `docs/pageupdater.md` for page content and edit flow",
      "Top-level `README.md`, `INSTALL`, `UPGRADE`, `DEVELOPERS.md` for usage and contributor workflow"
    ],
    "entrypoints": [
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/index.php#L13` for browser/page requests",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/api.php` for the Action API",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/rest.php` for the REST API",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/load.php` for ResourceLoader frontend assets",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/mw-config/index.php` for the web installer/configurator",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/maintenance/run.php` for CLI maintenance scripts"
    ],
    "important_docs": [
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/README.md`",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/DEVELOPERS.md`",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/docs/Injection.md`",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/docs/Hooks.md`",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/docs/database.md`",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/docs/contenthandler.md`",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/docs/pageupdater.md`",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/maintenance/README`"
    ],
    "key_configs": [
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/composer.json` for PHP deps, scripts, and local dev commands",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/package.json` for JS test/tooling commands",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/Gruntfile.js` for lint/QUnit/Karma setup",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/tests/jest/jest.config.js` for focused Jest suites",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/docker-compose.yml` for local containerized development",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/includes/config-schema.php` and `docs/config-schema.yaml` for configuration schema",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/resources/Resources.php` for frontend module definitions",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/sql/tables.json` for canonical schema"
    ],
    "key_languages": [
      "PHP dominates the repository",
      "JavaScript is the main frontend/test language",
      "LESS and CSS are used for styling",
      "SQL defines schema and migrations",
      "JSON is heavily used for config, schemas, manifests, and localization data",
      "Vue single-file components exist for newer UI modules"
    ],
    "high_risk_areas": [
      "`includes/Setup.php` because it bootstraps the entire application and config lifecycle",
      "`includes/ServiceWiring.php` and `includes/MediaWikiServices.php` because they define global service construction and dependency boundaries",
      "`resources/Resources.php` and `includes/ResourceLoader/*` because frontend asset loading is custom and central to the UI",
      "`sql/` plus `includes/DB`, `includes/Page`, `includes/Revision`, `includes/Storage` because page/revision/storage behavior is tightly coupled to schema and replication assumptions",
      "`includes/Registration/*` because extension and skin loading is manifest-driven and affects startup behavior",
      "`includes/Parser/*` and content/revision flows because they drive rendering, edits, and cached derived data"
    ],
    "navigation_order": [
      "Read `README.md` and `DEVELOPERS.md` for context",
      "Inspect top-level entrypoints: `index.php`, `api.php`, `rest.php`, `load.php`, `maintenance/run.php`",
      "Read `includes/WebStart.php` and `includes/Setup.php` to understand startup",
      "Read `docs/Injection.md` and inspect `includes/MediaWikiServices.php` plus `includes/ServiceWiring.php`",
      "Map the domain area you care about under `includes/`",
      "For frontend work, inspect `resources/Resources.php` first, then `resources/src/...`",
      "For persistence/edit flow, read `docs/contenthandler.md`, `docs/pageupdater.md`, `docs/database.md`, then inspect `includes/Page`, `includes/Revision`, `includes/Storage`",
      "For extension points, read `docs/Hooks.md` and `includes/Registration/*`",
      "For execution and verification, inspect `composer.json`, `package.json`, and `tests/`"
    ],
    "representative_code_files": [
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/index.php`",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/includes/WebStart.php`",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/includes/Setup.php`",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/includes/MediaWikiServices.php`",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/includes/ServiceWiring.php`",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/includes/GlobalFunctions.php`",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/resources/Resources.php`",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/includes/Page/PageStore.php`",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/includes/Revision/RevisionStore.php`",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/mw-config/index.php`"
    ],
    "representative_docs": [
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/docs/Injection.md`",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/docs/Hooks.md`",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/docs/database.md`",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/docs/contenthandler.md`",
      "`/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Control/docs/pageupdater.md`"
    ],
    "evidence": [
      "`includes/Defines.php` defines `MW_VERSION` as `1.46.0-alpha`, so this is a current MediaWiki core checkout, not a small custom fork",
      "`composer.json` names the package `mediawiki/core` and requires PHP `>=8.2.0`",
      "Top-level web entrypoints are thin wrappers that all hand off to shared infrastructure via `includes/WebStart.php` and service-based entrypoint classes",
      "`includes/Setup.php` explicitly documents the full bootstrap lifecycle: env checks, config loading, extension loading, service container init, exception handling, and session init",
      "`docs/Injection.md` describes `MediaWikiServices` as the heart of dependency injection, and `includes/ServiceWiring.php` is the concrete service map",
      "`includes/GlobalFunctions.php` exposes `wfLoadExtension()` and `wfLoadSkin()`, confirming manifest-driven extensibility",
      "`extensions/README` and `skins/README` show that extensions and skins are expected to be dropped in separately; this checkout mainly contains core",
      "`resources/Resources.php` registers frontend modules, showing that MediaWiki uses its own ResourceLoader module system instead of a conventional SPA bundler",
      "`tests/` contains PHPUnit, parser, QUnit, Jest, Selenium, and API-testing trees, showing a broad verification strategy",
      "Directory counts show scale: about 3461 files in `includes/`, 2641 in `resources/`, 2665 in `tests/`, and 3154 in `languages/`"
    ]
  },
  "tool_calls": []
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
    "high_risk_areas": 0.0,
    "navigation_order": 0.0,
    "representative_code_files": 0.0,
    "representative_docs": 0.0
  },
  "weighted_score": 0.0,
  "max_score": 100,
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 0,
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 0.0,
    "normalized_score": 0.0,
    "normalization_delta": 0.0
  }
}
```

## Control (CTO on)

### Prompt

```text
Task: Explain this repo
Repository path: /Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control
Explore the repository and produce a structured explanation.
```

### Run Metrics

- command: `codex exec -s danger-full-access -c 'reasoning_effort="low"' --output-schema /tmp/aethyme-eval-output-schema.json -o /tmp/aethyme-eval-mw-low-control-cto-on-result.json - < /tmp/aethyme-eval-control-cto-on-prompt.txt`
- exit code: `0`
- input tokens: `200573`
- output tokens: `0`
- retries: `0`
- review burden: `0`
- wall time: `0.000s`

### Final Output Message

```text
tokens used
200573
```

### Structured Output

```json
{
  "repo_summary": "MediaWiki core: a large PHP application that serves wiki pages, APIs, asset bundles, installation flows, and operations tooling. The runtime is organized around a small set of root entrypoints that all converge into shared bootstrapping and a global service container. This checkout is the core product, not a site-specific app; extensions and skins are loaded from separate plugin directories.",
  "code_areas": [
    "HTTP entrypoints and request dispatch in `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/index.php`, `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/api.php`, `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/rest.php`, `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/load.php`",
    "Shared bootstrap and configuration loading in `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/WebStart.php` and `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/Setup.php`",
    "Service container and dependency wiring in `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/MediaWikiServices.php` and `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/ServiceWiring.php`",
    "Core domain modules under `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/` such as `Actions`, `Api`, `Auth`, `DB`, `Parser`, `Revision`, `Page`, `ResourceLoader`, `Rest`, `SpecialPage`",
    "Frontend modules and assets under `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/resources/`",
    "Installer UI in `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/mw-config/`",
    "Operations and admin scripts in `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/maintenance/`",
    "Database schemas and migrations in `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/sql/`",
    "Localization and message catalogs in `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/languages/`",
    "Test suites in `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/tests/`"
  ],
  "reference_areas": [
    "Developer setup in `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/DEVELOPERS.md`",
    "Top-level project overview in `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/README.md`",
    "Architecture docs in `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/docs/README` and `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/docs/Injection.md`",
    "Frontend API docs in `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/resources/README.md`",
    "Extension and skin notes in `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/extensions/README` and `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/skins/README`"
  ],
  "entrypoints": [
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/index.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/api.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/rest.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/load.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/mw-config/index.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/maintenance/run.php"
  ],
  "important_docs": [
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/README.md",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/DEVELOPERS.md",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/docs/README",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/docs/Injection.md",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/resources/README.md",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/phpunit.xml.template"
  ],
  "key_configs": [
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/composer.json",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/package.json",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/Gruntfile.js",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/docker-compose.yml",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/phpunit.xml.template"
  ],
  "key_languages": [
    "PHP",
    "JavaScript",
    "Vue",
    "JSON",
    "SQL",
    "LESS/CSS"
  ],
  "high_risk_areas": [
    "Bootstrapping and config loading in `includes/Setup.php` because it defines process-wide initialization and extension loading",
    "Service wiring in `includes/ServiceWiring.php` because cross-cutting dependencies are assembled there",
    "Database schema and migrations in `sql/` because changes affect upgrades and multiple DB backends",
    "Request entrypoints because small changes can affect every web request",
    "Legacy/global-state-heavy areas under `includes/` where MediaWiki still mixes DI with older patterns"
  ],
  "navigation_order": [
    "Start with `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/README.md` and `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/DEVELOPERS.md`",
    "Read `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/index.php`, `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/api.php`, `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/rest.php`, and `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/load.php`",
    "Follow into `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/WebStart.php` and `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/Setup.php`",
    "Then inspect `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/MediaWikiServices.php` and `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/ServiceWiring.php`",
    "After that, browse the relevant subsystem in `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/` such as `Api`, `Rest`, `Parser`, `Page`, or `ResourceLoader`",
    "Use `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/resources/`, `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/tests/`, `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/maintenance/`, and `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/sql/` based on what you need"
  ],
  "representative_code_files": [
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/index.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/WebStart.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/Setup.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/Actions/ActionEntryPoint.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/Api/ApiEntryPoint.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/Rest/EntryPoint.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/ResourceLoader/ResourceLoaderEntryPoint.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/ServiceWiring.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/maintenance/run.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/mw-config/index.php"
  ],
  "representative_docs": [
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/README.md",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/DEVELOPERS.md",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/docs/Injection.md",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/resources/README.md"
  ],
  "evidence": [
    "`index.php`, `api.php`, `rest.php`, and `load.php` all define `MW_ENTRY_POINT`, require `includes/WebStart.php`, then instantiate a specialized entrypoint object.",
    "`includes/WebStart.php` handles web-specific checks and delegates to `includes/Setup.php`.",
    "`includes/Setup.php` explicitly says it initializes configuration, extensions, services, exception handling, and sessions for both web and CLI processes.",
    "`includes/Setup.php` sets `ServiceWiringFiles` to `includes/ServiceWiring.php`, confirming the service-container pattern.",
    "`docs/Injection.md` describes `MediaWikiServices` as the central service locator and `ServiceWiring.php` as the default wiring source.",
    "`maintenance/run.php` is the wrapper entrypoint for CLI maintenance scripts.",
    "`mw-config/index.php` is the installer entrypoint.",
    "`composer.json` defines PHP/server/test/lint scripts; `package.json` and `Gruntfile.js` define frontend linting and browser test workflows.",
    "`docker-compose.yml` and `DEVELOPERS.md` show the standard local dev environment is Docker-based with PHP, Apache, SQLite, Xdebug, and a jobrunner.",
    "`extensions/README` and `skins/README` state extensions and skins are distributed separately, so this repo is the core platform."
  ]
}
```

### Raw Run Record

```json
{
  "command": "codex exec -s danger-full-access -c 'reasoning_effort=\"low\"' --output-schema /tmp/aethyme-eval-output-schema.json -o /tmp/aethyme-eval-mw-low-control-cto-on-result.json - < /tmp/aethyme-eval-control-cto-on-prompt.txt",
  "exit_code": 0,
  "input_tokens": 200573,
  "output_tokens": 0,
  "retries": 0,
  "review_burden": 0,
  "duration_seconds": 0.0,
  "final_output_message": "tokens used\n200573",
  "structured_output": {
    "repo_summary": "MediaWiki core: a large PHP application that serves wiki pages, APIs, asset bundles, installation flows, and operations tooling. The runtime is organized around a small set of root entrypoints that all converge into shared bootstrapping and a global service container. This checkout is the core product, not a site-specific app; extensions and skins are loaded from separate plugin directories.",
    "code_areas": [
      "HTTP entrypoints and request dispatch in `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/index.php`, `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/api.php`, `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/rest.php`, `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/load.php`",
      "Shared bootstrap and configuration loading in `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/WebStart.php` and `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/Setup.php`",
      "Service container and dependency wiring in `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/MediaWikiServices.php` and `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/ServiceWiring.php`",
      "Core domain modules under `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/` such as `Actions`, `Api`, `Auth`, `DB`, `Parser`, `Revision`, `Page`, `ResourceLoader`, `Rest`, `SpecialPage`",
      "Frontend modules and assets under `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/resources/`",
      "Installer UI in `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/mw-config/`",
      "Operations and admin scripts in `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/maintenance/`",
      "Database schemas and migrations in `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/sql/`",
      "Localization and message catalogs in `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/languages/`",
      "Test suites in `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/tests/`"
    ],
    "reference_areas": [
      "Developer setup in `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/DEVELOPERS.md`",
      "Top-level project overview in `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/README.md`",
      "Architecture docs in `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/docs/README` and `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/docs/Injection.md`",
      "Frontend API docs in `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/resources/README.md`",
      "Extension and skin notes in `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/extensions/README` and `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/skins/README`"
    ],
    "entrypoints": [
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/index.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/api.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/rest.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/load.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/mw-config/index.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/maintenance/run.php"
    ],
    "important_docs": [
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/README.md",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/DEVELOPERS.md",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/docs/README",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/docs/Injection.md",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/resources/README.md",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/phpunit.xml.template"
    ],
    "key_configs": [
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/composer.json",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/package.json",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/Gruntfile.js",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/docker-compose.yml",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/phpunit.xml.template"
    ],
    "key_languages": [
      "PHP",
      "JavaScript",
      "Vue",
      "JSON",
      "SQL",
      "LESS/CSS"
    ],
    "high_risk_areas": [
      "Bootstrapping and config loading in `includes/Setup.php` because it defines process-wide initialization and extension loading",
      "Service wiring in `includes/ServiceWiring.php` because cross-cutting dependencies are assembled there",
      "Database schema and migrations in `sql/` because changes affect upgrades and multiple DB backends",
      "Request entrypoints because small changes can affect every web request",
      "Legacy/global-state-heavy areas under `includes/` where MediaWiki still mixes DI with older patterns"
    ],
    "navigation_order": [
      "Start with `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/README.md` and `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/DEVELOPERS.md`",
      "Read `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/index.php`, `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/api.php`, `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/rest.php`, and `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/load.php`",
      "Follow into `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/WebStart.php` and `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/Setup.php`",
      "Then inspect `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/MediaWikiServices.php` and `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/ServiceWiring.php`",
      "After that, browse the relevant subsystem in `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/` such as `Api`, `Rest`, `Parser`, `Page`, or `ResourceLoader`",
      "Use `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/resources/`, `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/tests/`, `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/maintenance/`, and `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/sql/` based on what you need"
    ],
    "representative_code_files": [
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/index.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/WebStart.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/Setup.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/Actions/ActionEntryPoint.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/Api/ApiEntryPoint.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/Rest/EntryPoint.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/ResourceLoader/ResourceLoaderEntryPoint.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/includes/ServiceWiring.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/maintenance/run.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/mw-config/index.php"
    ],
    "representative_docs": [
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/README.md",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/DEVELOPERS.md",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/docs/Injection.md",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/resources/README.md"
    ],
    "evidence": [
      "`index.php`, `api.php`, `rest.php`, and `load.php` all define `MW_ENTRY_POINT`, require `includes/WebStart.php`, then instantiate a specialized entrypoint object.",
      "`includes/WebStart.php` handles web-specific checks and delegates to `includes/Setup.php`.",
      "`includes/Setup.php` explicitly says it initializes configuration, extensions, services, exception handling, and sessions for both web and CLI processes.",
      "`includes/Setup.php` sets `ServiceWiringFiles` to `includes/ServiceWiring.php`, confirming the service-container pattern.",
      "`docs/Injection.md` describes `MediaWikiServices` as the central service locator and `ServiceWiring.php` as the default wiring source.",
      "`maintenance/run.php` is the wrapper entrypoint for CLI maintenance scripts.",
      "`mw-config/index.php` is the installer entrypoint.",
      "`composer.json` defines PHP/server/test/lint scripts; `package.json` and `Gruntfile.js` define frontend linting and browser test workflows.",
      "`docker-compose.yml` and `DEVELOPERS.md` show the standard local dev environment is Docker-based with PHP, Apache, SQLite, Xdebug, and a jobrunner.",
      "`extensions/README` and `skins/README` state extensions and skins are distributed separately, so this repo is the core platform."
    ]
  },
  "tool_calls": []
}
```

### Assessment

```json
{
  "scores": {
    "code_areas": 0.0,
    "reference_areas": 0.0,
    "entrypoints": 0.0,
    "important_docs": 0.3333333333333333,
    "key_configs": 0.0,
    "key_languages": 0.0,
    "high_risk_areas": 0.0,
    "navigation_order": 0.0,
    "representative_code_files": 0.0,
    "representative_docs": 0.3333333333333333
  },
  "weighted_score": 5.67,
  "max_score": 100,
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 31,
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 0.0,
    "normalized_score": 5.67,
    "normalization_delta": 5.67
  }
}
```

## Explore

### Prompt

```text
Task: Explain this repo
Repository path: /Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme
Explore the repository and produce a structured explanation.
```

### Run Metrics

- command: `codex exec -s danger-full-access -c 'reasoning_effort="low"' --output-schema /tmp/aethyme-eval-output-schema.json -o /tmp/aethyme-eval-mw-low-explore-result.json - < /tmp/aethyme-eval-explore-prompt.txt`
- exit code: `0`
- input tokens: `182222`
- output tokens: `0`
- retries: `0`
- review burden: `0`
- wall time: `0.000s`

### Final Output Message

```text
tokens used
182222
```

### Structured Output

```json
{
  "repo_summary": "MediaWiki core is the main wiki application platform behind Wikipedia and related projects. This repository is not a small app; it is the core runtime, installer, CLI tooling, frontend module system, configuration schema, and test infrastructure for running and extending a wiki.",
  "code_areas": [
    "`includes/`: main PHP backend code, including request handling, APIs, parser, storage, permissions, users, jobs, revision/page logic, and the service container",
    "`resources/src/`: frontend ResourceLoader modules shipped to browsers",
    "`resources/lib/`: vendored frontend libraries",
    "`languages/`: localisation data and message bundles",
    "`maintenance/`: CLI maintenance scripts and supporting infrastructure",
    "`mw-config/`: web installer/configuration entrypoint",
    "`sql/`: schema and database artifacts",
    "`tests/`: PHPUnit, API, Jest, QUnit, Selenium, parser, and static-analysis test suites"
  ],
  "reference_areas": [
    "`docs/`: architecture and subsystem docs such as DI, hooks, content handling, database access, page updates, and events",
    "`extensions/`: install location for separately distributed extensions",
    "`skins/`: install location for separately distributed skins"
  ],
  "entrypoints": [
    "[index.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/index.php#L13) for normal browser/page requests",
    "[api.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/api.php#L2) for the Action API",
    "[rest.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/rest.php#L3) for the REST API",
    "[load.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/load.php#L2) for ResourceLoader asset delivery",
    "[maintenance/run.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/maintenance/run.php#L3) for CLI maintenance commands",
    "[mw-config/index.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/mw-config/index.php#L3) for installation/setup"
  ],
  "important_docs": [
    "[README.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/README.md#L1)",
    "[DEVELOPERS.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/DEVELOPERS.md#L1)",
    "[docs/Injection.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/Injection.md#L1)",
    "[docs/Hooks.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/Hooks.md#L1)",
    "[docs/Events.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/Events.md#L1)",
    "[docs/contenthandler.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/contenthandler.md#L1)",
    "[docs/pageupdater.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/pageupdater.md#L1)",
    "[resources/README.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/resources/README.md#L1)",
    "[maintenance/README](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/maintenance/README#L1)"
  ],
  "key_configs": [
    "[composer.json](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/composer.json#L1)",
    "[package.json](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/package.json#L1)",
    "[docker-compose.yml](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docker-compose.yml#L1)",
    "[Gruntfile.js](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/Gruntfile.js#L1)",
    "[tests/jest/jest.config.js](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/tests/jest/jest.config.js#L1)",
    "[tests/selenium/wdio.conf.js](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/tests/selenium/wdio.conf.js#L1)",
    "[.phan/config.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/.phan/config.php#L1)",
    "[includes/config-schema.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/config-schema.php#L1)"
  ],
  "key_languages": [
    "PHP",
    "JavaScript",
    "Vue",
    "JSON/YAML config"
  ],
  "high_risk_areas": [
    "Bootstrap and global initialization in [includes/Setup.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/Setup.php#L1) because it controls config loading, extension registration, service initialization, sessions, hooks, and request globals",
    "Generated autoload/config schema artifacts such as [autoload.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/autoload.php#L1) and [includes/config-schema.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/config-schema.php#L1), which should not be edited manually",
    "The service container and wiring in [includes/MediaWikiServices.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/MediaWikiServices.php#L250) and [includes/ServiceWiring.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/ServiceWiring.php#L1) because changes there ripple widely",
    "Edit and content pipelines documented in [docs/contenthandler.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/contenthandler.md#L28) and [docs/pageupdater.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/pageupdater.md#L6), since they affect storage, rendering, caching, and revision semantics"
  ],
  "navigation_order": [
    "Read [README.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/README.md#L1) and [DEVELOPERS.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/DEVELOPERS.md#L1)",
    "Trace the runtime from [index.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/index.php#L13), [api.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/api.php#L2), [rest.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/rest.php#L3), and [load.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/load.php#L2) into [includes/WebStart.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/WebStart.php#L2) and [includes/Setup.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/Setup.php#L2)",
    "Understand dependency injection via [docs/Injection.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/Injection.md#L22), [includes/MediaWikiServices.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/MediaWikiServices.php#L250), and [includes/ServiceWiring.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/ServiceWiring.php#L1)",
    "Review extensibility via [docs/Hooks.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/Hooks.md#L34) and [docs/Events.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/Events.md#L24)",
    "Review content/edit pipeline via [docs/contenthandler.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/contenthandler.md#L28) and [docs/pageupdater.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/pageupdater.md#L6)",
    "Then go into the specific area you care about under `includes/`, `resources/src/`, `maintenance/`, or `tests/`"
  ],
  "representative_code_files": [
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/index.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/api.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/rest.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/load.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/includes/WebStart.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/includes/Setup.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/includes/AutoLoader.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/includes/MediaWikiServices.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/includes/ServiceWiring.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/maintenance/run.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/mw-config/index.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/Gruntfile.js"
  ],
  "representative_docs": [
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/README.md",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/DEVELOPERS.md",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/docs/Injection.md",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/docs/Hooks.md",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/docs/Events.md",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/docs/contenthandler.md",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/docs/pageupdater.md",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/resources/README.md",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/maintenance/README"
  ],
  "evidence": [
    "[README.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/README.md#L1) describes the repo as MediaWiki, a PHP wiki platform used by Wikipedia",
    "[composer.json](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/composer.json#L23) shows PHP 8.2+, core runtime dependencies, and Composer scripts for serving, linting, PHPUnit, and maintenance",
    "[package.json](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/package.json#L4) shows frontend/test tooling: Grunt, Jest, QUnit, Selenium, Vue, and Codex",
    "[index.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/index.php#L13), [api.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/api.php#L2), [rest.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/rest.php#L3), and [load.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/load.php#L2) establish the main runtime entrypoints",
    "[includes/WebStart.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/WebStart.php#L2) and [includes/Setup.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/Setup.php#L18) define the common bootstrap path for web and CLI",
    "[includes/Setup.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/Setup.php#L175) sets extension/skin/image dirs and the default service wiring file; [includes/Setup.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/Setup.php#L279) loads extensions; [includes/Setup.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/Setup.php#L323) enables the global service instance; [includes/Setup.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/Setup.php#L483) initializes sessions",
    "[includes/MediaWikiServices.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/MediaWikiServices.php#L250) and [docs/Injection.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/Injection.md#L24) identify `MediaWikiServices` plus [includes/ServiceWiring.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/ServiceWiring.php#L1) as the DI backbone",
    "[docs/Hooks.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/Hooks.md#L34) explains the hook system and [docs/Events.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/Events.md#L24) shows the newer domain event direction",
    "[docs/contenthandler.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/contenthandler.md#L28) and [docs/pageupdater.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/pageupdater.md#L6) document the core content/edit model",
    "[includes/AutoLoader.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/AutoLoader.php#L19) and [autoload.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/autoload.php#L1) show that core uses a generated class map rather than full PSR-4 for performance",
    "[includes/config-schema.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/config-schema.php#L1) is generated, reinforcing that some important artifacts are build outputs",
    "[maintenance/README](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/maintenance/README#L6) and [maintenance/run.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/maintenance/run.php#L3) define the maintenance script system",
    "[resources/README.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/resources/README.md#L1), [Gruntfile.js](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/Gruntfile.js#L49), [tests/jest/jest.config.js](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/tests/jest/jest.config.js#L20), and [tests/selenium/wdio.conf.js](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/tests/selenium/wdio.conf.js#L1) show the frontend module system and JS test stack",
    "[docker-compose.yml](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docker-compose.yml#L7) and [DEVELOPERS.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/DEVELOPERS.md#L7) describe the local Docker-based development environment",
    "[extensions/README](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/extensions/README#L1) and [skins/README](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/skins/README#L1) show that most extensions/skins are installed separately, not developed inline by default"
  ]
}
```

### Raw Run Record

```json
{
  "command": "codex exec -s danger-full-access -c 'reasoning_effort=\"low\"' --output-schema /tmp/aethyme-eval-output-schema.json -o /tmp/aethyme-eval-mw-low-explore-result.json - < /tmp/aethyme-eval-explore-prompt.txt",
  "exit_code": 0,
  "input_tokens": 182222,
  "output_tokens": 0,
  "retries": 0,
  "review_burden": 0,
  "duration_seconds": 0.0,
  "final_output_message": "tokens used\n182222",
  "structured_output": {
    "repo_summary": "MediaWiki core is the main wiki application platform behind Wikipedia and related projects. This repository is not a small app; it is the core runtime, installer, CLI tooling, frontend module system, configuration schema, and test infrastructure for running and extending a wiki.",
    "code_areas": [
      "`includes/`: main PHP backend code, including request handling, APIs, parser, storage, permissions, users, jobs, revision/page logic, and the service container",
      "`resources/src/`: frontend ResourceLoader modules shipped to browsers",
      "`resources/lib/`: vendored frontend libraries",
      "`languages/`: localisation data and message bundles",
      "`maintenance/`: CLI maintenance scripts and supporting infrastructure",
      "`mw-config/`: web installer/configuration entrypoint",
      "`sql/`: schema and database artifacts",
      "`tests/`: PHPUnit, API, Jest, QUnit, Selenium, parser, and static-analysis test suites"
    ],
    "reference_areas": [
      "`docs/`: architecture and subsystem docs such as DI, hooks, content handling, database access, page updates, and events",
      "`extensions/`: install location for separately distributed extensions",
      "`skins/`: install location for separately distributed skins"
    ],
    "entrypoints": [
      "[index.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/index.php#L13) for normal browser/page requests",
      "[api.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/api.php#L2) for the Action API",
      "[rest.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/rest.php#L3) for the REST API",
      "[load.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/load.php#L2) for ResourceLoader asset delivery",
      "[maintenance/run.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/maintenance/run.php#L3) for CLI maintenance commands",
      "[mw-config/index.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/mw-config/index.php#L3) for installation/setup"
    ],
    "important_docs": [
      "[README.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/README.md#L1)",
      "[DEVELOPERS.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/DEVELOPERS.md#L1)",
      "[docs/Injection.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/Injection.md#L1)",
      "[docs/Hooks.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/Hooks.md#L1)",
      "[docs/Events.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/Events.md#L1)",
      "[docs/contenthandler.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/contenthandler.md#L1)",
      "[docs/pageupdater.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/pageupdater.md#L1)",
      "[resources/README.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/resources/README.md#L1)",
      "[maintenance/README](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/maintenance/README#L1)"
    ],
    "key_configs": [
      "[composer.json](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/composer.json#L1)",
      "[package.json](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/package.json#L1)",
      "[docker-compose.yml](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docker-compose.yml#L1)",
      "[Gruntfile.js](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/Gruntfile.js#L1)",
      "[tests/jest/jest.config.js](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/tests/jest/jest.config.js#L1)",
      "[tests/selenium/wdio.conf.js](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/tests/selenium/wdio.conf.js#L1)",
      "[.phan/config.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/.phan/config.php#L1)",
      "[includes/config-schema.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/config-schema.php#L1)"
    ],
    "key_languages": [
      "PHP",
      "JavaScript",
      "Vue",
      "JSON/YAML config"
    ],
    "high_risk_areas": [
      "Bootstrap and global initialization in [includes/Setup.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/Setup.php#L1) because it controls config loading, extension registration, service initialization, sessions, hooks, and request globals",
      "Generated autoload/config schema artifacts such as [autoload.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/autoload.php#L1) and [includes/config-schema.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/config-schema.php#L1), which should not be edited manually",
      "The service container and wiring in [includes/MediaWikiServices.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/MediaWikiServices.php#L250) and [includes/ServiceWiring.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/ServiceWiring.php#L1) because changes there ripple widely",
      "Edit and content pipelines documented in [docs/contenthandler.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/contenthandler.md#L28) and [docs/pageupdater.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/pageupdater.md#L6), since they affect storage, rendering, caching, and revision semantics"
    ],
    "navigation_order": [
      "Read [README.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/README.md#L1) and [DEVELOPERS.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/DEVELOPERS.md#L1)",
      "Trace the runtime from [index.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/index.php#L13), [api.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/api.php#L2), [rest.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/rest.php#L3), and [load.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/load.php#L2) into [includes/WebStart.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/WebStart.php#L2) and [includes/Setup.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/Setup.php#L2)",
      "Understand dependency injection via [docs/Injection.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/Injection.md#L22), [includes/MediaWikiServices.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/MediaWikiServices.php#L250), and [includes/ServiceWiring.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/ServiceWiring.php#L1)",
      "Review extensibility via [docs/Hooks.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/Hooks.md#L34) and [docs/Events.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/Events.md#L24)",
      "Review content/edit pipeline via [docs/contenthandler.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/contenthandler.md#L28) and [docs/pageupdater.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/pageupdater.md#L6)",
      "Then go into the specific area you care about under `includes/`, `resources/src/`, `maintenance/`, or `tests/`"
    ],
    "representative_code_files": [
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/index.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/api.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/rest.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/load.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/includes/WebStart.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/includes/Setup.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/includes/AutoLoader.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/includes/MediaWikiServices.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/includes/ServiceWiring.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/maintenance/run.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/mw-config/index.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/Gruntfile.js"
    ],
    "representative_docs": [
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/README.md",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/DEVELOPERS.md",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/docs/Injection.md",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/docs/Hooks.md",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/docs/Events.md",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/docs/contenthandler.md",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/docs/pageupdater.md",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/resources/README.md",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/maintenance/README"
    ],
    "evidence": [
      "[README.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/README.md#L1) describes the repo as MediaWiki, a PHP wiki platform used by Wikipedia",
      "[composer.json](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/composer.json#L23) shows PHP 8.2+, core runtime dependencies, and Composer scripts for serving, linting, PHPUnit, and maintenance",
      "[package.json](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/package.json#L4) shows frontend/test tooling: Grunt, Jest, QUnit, Selenium, Vue, and Codex",
      "[index.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/index.php#L13), [api.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/api.php#L2), [rest.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/rest.php#L3), and [load.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/load.php#L2) establish the main runtime entrypoints",
      "[includes/WebStart.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/WebStart.php#L2) and [includes/Setup.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/Setup.php#L18) define the common bootstrap path for web and CLI",
      "[includes/Setup.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/Setup.php#L175) sets extension/skin/image dirs and the default service wiring file; [includes/Setup.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/Setup.php#L279) loads extensions; [includes/Setup.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/Setup.php#L323) enables the global service instance; [includes/Setup.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/Setup.php#L483) initializes sessions",
      "[includes/MediaWikiServices.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/MediaWikiServices.php#L250) and [docs/Injection.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/Injection.md#L24) identify `MediaWikiServices` plus [includes/ServiceWiring.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/ServiceWiring.php#L1) as the DI backbone",
      "[docs/Hooks.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/Hooks.md#L34) explains the hook system and [docs/Events.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/Events.md#L24) shows the newer domain event direction",
      "[docs/contenthandler.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/contenthandler.md#L28) and [docs/pageupdater.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docs/pageupdater.md#L6) document the core content/edit model",
      "[includes/AutoLoader.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/AutoLoader.php#L19) and [autoload.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/autoload.php#L1) show that core uses a generated class map rather than full PSR-4 for performance",
      "[includes/config-schema.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/includes/config-schema.php#L1) is generated, reinforcing that some important artifacts are build outputs",
      "[maintenance/README](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/maintenance/README#L6) and [maintenance/run.php](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/maintenance/run.php#L3) define the maintenance script system",
      "[resources/README.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/resources/README.md#L1), [Gruntfile.js](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/Gruntfile.js#L49), [tests/jest/jest.config.js](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/tests/jest/jest.config.js#L20), and [tests/selenium/wdio.conf.js](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/tests/selenium/wdio.conf.js#L1) show the frontend module system and JS test stack",
      "[docker-compose.yml](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/docker-compose.yml#L7) and [DEVELOPERS.md](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/DEVELOPERS.md#L7) describe the local Docker-based development environment",
      "[extensions/README](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/extensions/README#L1) and [skins/README](/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki%20-%20Aethyme/skins/README#L1) show that most extensions/skins are installed separately, not developed inline by default"
    ]
  },
  "tool_calls": []
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
    "high_risk_areas": 0.0,
    "navigation_order": 0.0,
    "representative_code_files": 0.0,
    "representative_docs": 0.3333333333333333
  },
  "weighted_score": 0.67,
  "max_score": 100,
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 38,
      "markdown_link_count": 17,
      "line_anchor_count": 0
    },
    "raw_score": 0.0,
    "normalized_score": 0.67,
    "normalization_delta": 0.67
  }
}
```

## Leverage

### Prompt

```text
Task: Explain this repo
Repository path: /Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme
Use Aethyme tools to navigate the repository graph. Explore the repository and produce a structured explanation.
```

### Run Metrics

- command: `codex exec -s danger-full-access -c 'reasoning_effort="low"' --output-schema /tmp/aethyme-eval-output-schema.json -o /tmp/aethyme-eval-mw-low-leverage-result.json - < /tmp/aethyme-eval-leverage-prompt.txt`
- exit code: `0`
- input tokens: `117226`
- output tokens: `0`
- retries: `0`
- review burden: `0`
- wall time: `0.000s`

### Final Output Message

```text
tokens used
117226
```

### Structured Output

```json
{
  "repo_summary": "MediaWiki core is a large PHP-first wiki engine with a browser UI, Action API, REST API, asset loader, installer, maintenance CLI, and a substantial frontend/resource layer. The runtime starts from small root entrypoints like index.php/api.php/rest.php/load.php/img_auth.php, which all converge through includes/WebStart.php and includes/Setup.php to load configuration, extensions, and the central service container.",
  "code_areas": [
    "Backend/runtime in includes/ and autoload.php-generated class map",
    "Frontend assets and public JS API in resources/, resources/src/, resources/lib/, and resources/README.md",
    "Installer UI in mw-config/ with browser-side behavior in mw-config/config.js",
    "CLI/ops scripts in maintenance/, especially maintenance/run.php and maintenance/update.php",
    "Tests split across tests/phpunit, tests/jest, tests/qunit, tests/selenium, and tests/api-testing"
  ],
  "reference_areas": [
    "extensions/ and skins/ are core integration surfaces for pluggable features/themes",
    "languages/ holds localization/i18n data consumed across the app",
    "images/ is the default upload area referenced during setup"
  ],
  "entrypoints": [
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/index.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/api.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/rest.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/load.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/img_auth.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/maintenance/run.php"
  ],
  "important_docs": [
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/README.md",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/DEVELOPERS.md",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/resources/README.md",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/resources/src/README.md",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/maintenance/README",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/tests/phpunit/README.md",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/tests/selenium/README.md"
  ],
  "key_configs": [
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/composer.json",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/package.json",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/docker-compose.yml",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/Gruntfile.js",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/phpunit.xml.template",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/tests/jest/jest.config.js"
  ],
  "key_languages": [
    "PHP",
    "JavaScript",
    "Less/CSS",
    "JSON",
    "Mustache/templates",
    "SQL"
  ],
  "high_risk_areas": [
    "Bootstrap/config loading in includes/WebStart.php and includes/Setup.php",
    "The global service container in includes/MediaWikiServices.php",
    "Generated autoload map in autoload.php",
    "Schema/upgrade path in maintenance/update.php",
    "Broad extension/skin integration surface referenced throughout setup and tests"
  ],
  "navigation_order": [
    "Start with README.md and DEVELOPERS.md",
    "Read root entrypoints: index.php, api.php, rest.php, load.php, img_auth.php",
    "Follow bootstrap through includes/WebStart.php and includes/Setup.php",
    "Use MediaWikiServices.php and autoload.php to understand backend structure",
    "Then inspect resources/, mw-config/, maintenance/, and tests/ depending on your focus"
  ],
  "representative_code_files": [
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/index.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/includes/WebStart.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/includes/Setup.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/includes/MediaWikiServices.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/autoload.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/mw-config/config.js",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/maintenance/run.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/maintenance/update.php",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/tests/jest/jest.config.js",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/tests/selenium/wdio-mediawiki/index.js"
  ],
  "representative_docs": [
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/README.md",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/DEVELOPERS.md",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/resources/README.md",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/resources/src/README.md",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/maintenance/README",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/tests/phpunit/README.md",
    "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/tests/selenium/README.md"
  ],
  "evidence": [
    "Aethyme `task explain` identified a very large mixed PHP/JS repo and surfaced `resources`, `tests`, and `maintenance` as the clearest graph areas.",
    "Aethyme `graph overview` reported strong entrypoint clarity but weaker parser visibility, so direct source reads were needed for the PHP bootstrap path.",
    "README.md describes the repo as MediaWiki core, the software behind Wikipedia and other Wikimedia sites.",
    "index.php, api.php, rest.php, load.php, and img_auth.php are thin entrypoints that all require includes/WebStart.php and then dispatch to specialized entrypoint classes.",
    "includes/WebStart.php performs web-specific checks, locates LocalSettings.php, sets up output buffering, and then requires includes/Setup.php.",
    "includes/Setup.php is the real bootstrap: it loads autoloaders, default config, LocalSettings, extensions, runtime initialization, and service wiring.",
    "includes/MediaWikiServices.php is the central dependency-injection/service-container definition for the application.",
    "autoload.php is generated and maps a huge number of PHP classes across includes/ and maintenance/, showing that includes/ is the main backend code surface.",
    "composer.json defines the PHP runtime/dependencies and scripts for linting, phan, phpcs, phpunit, install, serving, and maintenance commands.",
    "package.json plus Gruntfile.js define the frontend/tooling side: ESLint, Stylelint, Banana i18n checks, Karma/QUnit, Jest, JSDoc, SVGO, and Selenium.",
    "docker-compose.yml and DEVELOPERS.md show the intended local-dev workflow: Docker Compose with PHP/Apache/jobrunner and a default SQLite-backed setup.",
    "maintenance/run.php and maintenance/README show the generic CLI runner model, while maintenance/update.php handles schema upgrades.",
    "resources/README.md and resources/src/README.md describe the frontend API and module surface available to user scripts, gadgets, skins, and extensions.",
    "tests are intentionally multi-layered: phpunit.xml.template and tests/phpunit/README.md for PHP suites, tests/jest/jest.config.js for targeted JS/Vue unit coverage, and tests/selenium/README.md plus tests/selenium/wdio-mediawiki/index.js for browser automation."
  ]
}
```

### Raw Run Record

```json
{
  "command": "codex exec -s danger-full-access -c 'reasoning_effort=\"low\"' --output-schema /tmp/aethyme-eval-output-schema.json -o /tmp/aethyme-eval-mw-low-leverage-result.json - < /tmp/aethyme-eval-leverage-prompt.txt",
  "exit_code": 0,
  "input_tokens": 117226,
  "output_tokens": 0,
  "retries": 0,
  "review_burden": 0,
  "duration_seconds": 0.0,
  "final_output_message": "tokens used\n117226",
  "structured_output": {
    "repo_summary": "MediaWiki core is a large PHP-first wiki engine with a browser UI, Action API, REST API, asset loader, installer, maintenance CLI, and a substantial frontend/resource layer. The runtime starts from small root entrypoints like index.php/api.php/rest.php/load.php/img_auth.php, which all converge through includes/WebStart.php and includes/Setup.php to load configuration, extensions, and the central service container.",
    "code_areas": [
      "Backend/runtime in includes/ and autoload.php-generated class map",
      "Frontend assets and public JS API in resources/, resources/src/, resources/lib/, and resources/README.md",
      "Installer UI in mw-config/ with browser-side behavior in mw-config/config.js",
      "CLI/ops scripts in maintenance/, especially maintenance/run.php and maintenance/update.php",
      "Tests split across tests/phpunit, tests/jest, tests/qunit, tests/selenium, and tests/api-testing"
    ],
    "reference_areas": [
      "extensions/ and skins/ are core integration surfaces for pluggable features/themes",
      "languages/ holds localization/i18n data consumed across the app",
      "images/ is the default upload area referenced during setup"
    ],
    "entrypoints": [
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/index.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/api.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/rest.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/load.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/img_auth.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/maintenance/run.php"
    ],
    "important_docs": [
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/README.md",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/DEVELOPERS.md",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/resources/README.md",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/resources/src/README.md",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/maintenance/README",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/tests/phpunit/README.md",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/tests/selenium/README.md"
    ],
    "key_configs": [
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/composer.json",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/package.json",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/docker-compose.yml",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/Gruntfile.js",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/phpunit.xml.template",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/tests/jest/jest.config.js"
    ],
    "key_languages": [
      "PHP",
      "JavaScript",
      "Less/CSS",
      "JSON",
      "Mustache/templates",
      "SQL"
    ],
    "high_risk_areas": [
      "Bootstrap/config loading in includes/WebStart.php and includes/Setup.php",
      "The global service container in includes/MediaWikiServices.php",
      "Generated autoload map in autoload.php",
      "Schema/upgrade path in maintenance/update.php",
      "Broad extension/skin integration surface referenced throughout setup and tests"
    ],
    "navigation_order": [
      "Start with README.md and DEVELOPERS.md",
      "Read root entrypoints: index.php, api.php, rest.php, load.php, img_auth.php",
      "Follow bootstrap through includes/WebStart.php and includes/Setup.php",
      "Use MediaWikiServices.php and autoload.php to understand backend structure",
      "Then inspect resources/, mw-config/, maintenance/, and tests/ depending on your focus"
    ],
    "representative_code_files": [
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/index.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/includes/WebStart.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/includes/Setup.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/includes/MediaWikiServices.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/autoload.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/mw-config/config.js",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/maintenance/run.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/maintenance/update.php",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/tests/jest/jest.config.js",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/tests/selenium/wdio-mediawiki/index.js"
    ],
    "representative_docs": [
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/README.md",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/DEVELOPERS.md",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/resources/README.md",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/resources/src/README.md",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/maintenance/README",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/tests/phpunit/README.md",
      "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/tests/selenium/README.md"
    ],
    "evidence": [
      "Aethyme `task explain` identified a very large mixed PHP/JS repo and surfaced `resources`, `tests`, and `maintenance` as the clearest graph areas.",
      "Aethyme `graph overview` reported strong entrypoint clarity but weaker parser visibility, so direct source reads were needed for the PHP bootstrap path.",
      "README.md describes the repo as MediaWiki core, the software behind Wikipedia and other Wikimedia sites.",
      "index.php, api.php, rest.php, load.php, and img_auth.php are thin entrypoints that all require includes/WebStart.php and then dispatch to specialized entrypoint classes.",
      "includes/WebStart.php performs web-specific checks, locates LocalSettings.php, sets up output buffering, and then requires includes/Setup.php.",
      "includes/Setup.php is the real bootstrap: it loads autoloaders, default config, LocalSettings, extensions, runtime initialization, and service wiring.",
      "includes/MediaWikiServices.php is the central dependency-injection/service-container definition for the application.",
      "autoload.php is generated and maps a huge number of PHP classes across includes/ and maintenance/, showing that includes/ is the main backend code surface.",
      "composer.json defines the PHP runtime/dependencies and scripts for linting, phan, phpcs, phpunit, install, serving, and maintenance commands.",
      "package.json plus Gruntfile.js define the frontend/tooling side: ESLint, Stylelint, Banana i18n checks, Karma/QUnit, Jest, JSDoc, SVGO, and Selenium.",
      "docker-compose.yml and DEVELOPERS.md show the intended local-dev workflow: Docker Compose with PHP/Apache/jobrunner and a default SQLite-backed setup.",
      "maintenance/run.php and maintenance/README show the generic CLI runner model, while maintenance/update.php handles schema upgrades.",
      "resources/README.md and resources/src/README.md describe the frontend API and module surface available to user scripts, gadgets, skins, and extensions.",
      "tests are intentionally multi-layered: phpunit.xml.template and tests/phpunit/README.md for PHP suites, tests/jest/jest.config.js for targeted JS/Vue unit coverage, and tests/selenium/README.md plus tests/selenium/wdio-mediawiki/index.js for browser automation."
    ]
  },
  "tool_calls": []
}
```

### Assessment

```json
{
  "scores": {
    "code_areas": 0.0,
    "reference_areas": 0.0,
    "entrypoints": 0.0,
    "important_docs": 0.3333333333333333,
    "key_configs": 0.0,
    "key_languages": 0.0,
    "high_risk_areas": 0.0,
    "navigation_order": 0.0,
    "representative_code_files": 0.0,
    "representative_docs": 0.3333333333333333
  },
  "weighted_score": 5.67,
  "max_score": 100,
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 36,
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 0.0,
    "normalized_score": 5.67,
    "normalization_delta": 5.67
  }
}
```

## Comparison

| Metric | Control (CTO off) | Control (CTO on) | Explore | Leverage |
| --- | --- | --- | --- | --- |
| Prompt chars | `189` | `189` | `189` | `241` |
| Wall time | `0.0s` | `0.0s` | `0.0s` | `0.0s` |
| Input tokens | `181434` | `200573` | `182222` | `117226` |
| Output tokens | `0` | `0` | `0` | `0` |
| Score | `0.0` | `5.67` | `0.67` | `5.67` |

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
null
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
