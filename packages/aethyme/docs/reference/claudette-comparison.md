# Claudette vs Aethyme — Feature Comparison

> Analysis date: 2026-04-09
> Claudette repo: https://github.com/nicmarti/Claudette

## What is Claudette?

Claudette is an open-source, single-binary CLI tool written in **Go** that builds
a structural knowledge graph of a codebase using **Tree-sitter** parsing and stores
it in a local **SQLite** database. It integrates with Claude Code via **MCP**
(Model Context Protocol) so Claude can query code relationships without re-reading
entire files.

Created by Nicolas Martignole (Go rewrite of Tirth Kanani's "code-review-graph").
MIT licensed. Supports Python, JavaScript, TypeScript, and Go.

## Side-by-Side Feature Matrix

| Capability | **Claudette** | **Aethyme** |
|---|---|---|
| **Primary goal** | Give Claude Code structural context via MCP | GPS for AI agents navigating repositories — deterministic context, scope-aware routing |
| **Language** | Go (single binary, CGO for Tree-sitter/SQLite) | Python orchestration + Rust deterministic engine |
| **Parsing** | Tree-sitter only | SCIP (language-server-quality) + Tree-sitter fallback + regex heuristic fallback |
| **Supported languages** | Python, JS/TS, Go (4) | Python, JS/TS (SCIP); extensible via Rust engine |
| **Storage** | Local SQLite (`.claudette/graph.db`) | PostgreSQL with multi-tenant row-level security + Redis caching |
| **Graph model** | 5 node types (File, Class, Function, Type, Test) + 7 edge types (CALLS, IMPORTS_FROM, INHERITS, IMPLEMENTS, CONTAINS, TESTED_BY, DEPENDS_ON) | 8 node kinds (definition, reference, file, class, function, method, variable, import) + 8 edge types (invoke, import, contain, inherit, implement, props_flow, return, parameter) |
| **Incremental updates** | Git-diff-based, re-parses changed files + dependents, <2s | Git-snapshot-based deterministic caching (commit + dirty state + engine mtime) |
| **Blast-radius / Impact analysis** | BFS traversal, configurable depth (default 2 hops) | Dedicated `/api/v1/impact/` endpoint, depth up to 20, limit up to 5000 nodes |
| **Ego-graph traversal** | No dedicated feature | `/api/v1/ego/` with configurable depth (1-3) |
| **Search** | Name-based + optional vector embeddings for semantic search | Exact, fuzzy, and hybrid search via API and CLI |
| **Context pack assembly** | Token-optimized review context (~10KB cap, 3-line context windows) | Deterministic Rust-assembled context packs with anchors, navigation order, risk flags, scope |
| **Scope / risk classification** | No | Rust engine classifies scope and risk per task |
| **Task navigation** | No | Task anchors, next-step guidance, node expansion, scope determination |
| **Visualization** | D3.js force-directed graph (interactive HTML) | None |
| **Watch mode** | Filesystem watcher (fsnotify) + PostEdit/PostGit hooks | None; snapshot-based caching instead |
| **Agent integration** | MCP server (stdio transport, 8 tools) + 3 slash commands | FastAPI REST API with rate limiting, CORS, Prometheus metrics |
| **Authentication** | None (local-only, zero telemetry) | OIDC JWT + API keys, multi-tenant org/tenant/scope enforcement |
| **Multi-tenancy** | No (single-user local) | Platform > Org > Tenant > Repository > Graph hierarchy, row-level security |
| **Scorecard / code health** | No | AI-readiness scorecard with 8+ detectors (route coverage, i18n, schema drift, etc.) |
| **Autofixers** | No | Safety-checked patch generation, docs regeneration, link fixes, i18n scaffolding |
| **Evaluation framework** | No | explain-repo, bug-fix, navigation-CTF benchmarks with strict eval-integrity protocol |
| **SDK / client library** | No (MCP tools only) | Python SDK (`AethymeClient`) with search, ego-graph, impact, scorecard APIs |
| **Installation** | `go install` — single binary, zero runtime deps | Python package + Rust engine (auto-built on first use) |

## Where Claudette Excels

1. **Simplicity and zero friction** — Single Go binary, SQLite storage, no infrastructure. `claudette build` and done.
2. **MCP-native integration** — Purpose-built for Claude Code with 8 MCP tools and 3 slash commands (`/claudette:build-graph`, `/claudette:review-delta`, `/claudette:review-pr`).
3. **Watch mode and hooks** — Continuous background indexing via fsnotify, plus PostEdit and PostGit hooks for automatic updates.
4. **Interactive visualization** — D3.js force-directed graph with edge-type toggles and search.
5. **Token-conscious context** — Review context extracts only changed lines (3 lines above, 2 below), merges overlapping ranges, caps at ~10KB.
6. **Privacy** — Fully local, zero telemetry, zero network calls during operation.

## Where Aethyme Excels

1. **Depth of graph model** — Richer node/edge taxonomy and language-server-quality indexing via SCIP (not just structural Tree-sitter patterns).
2. **Deterministic context-pack assembly** — Rust engine builds task-specific packs with anchors, navigation order, risk flags, and scope — not just relevant code but navigation guidance.
3. **Task-aware navigation** — Anchors, scope determination, next-step guidance, and expansion are first-class concepts.
4. **Production-grade indexing** — SCIP as primary indexer provides accurate type-aware symbol resolution. Tree-sitter and regex as fallbacks.
5. **Multi-tenant SaaS architecture** — Row-level security, OIDC, API keys, org/tenant isolation.
6. **Scorecard and autofixers** — Proactive code health analysis and automated remediation.
7. **Evaluation framework** — Rigorous benchmarks with strict integrity rules (no eval-driven tool changes).
8. **Impact analysis depth** — Up to 20 levels deep vs Claudette's default 2-hop BFS.

## Key Architectural Differences

| Aspect | Claudette | Aethyme |
|---|---|---|
| **Philosophy** | Lightweight dev tool — "give Claude a map" | Platform infrastructure — "be the GPS" |
| **Deployment** | Local binary, no server needed | Client-server with API, multi-tenant DB |
| **Intelligence location** | Claude interprets the raw graph | Aethyme engine provides deterministic navigation guidance |
| **Symbol resolution** | Approximate name matching (no type resolution) | SCIP-based type-aware resolution |
| **Caching** | SQLite persistence + SHA-256 file hashes | Git-snapshot keyed caching + Redis + engine mtime |
| **Resilience** | Tolerates broken code (Tree-sitter is lenient) | SCIP requires compilable code; fallback indexers handle incomplete code |

## Potential Learnings from Claudette

1. **MCP-first integration** — Claudette's seamless Claude Code integration via MCP stdio server and slash commands is a UX advantage. Aethyme could expose its capabilities as MCP tools.
2. **Watch mode** — Continuous background indexing during development could complement Aethyme's snapshot-based caching for real-time feedback.
3. **Visualization** — Interactive D3.js graph exploration is a compelling developer experience feature Aethyme currently lacks.
4. **Zero-config local mode** — A SQLite-backed local mode (no PostgreSQL required) could lower Aethyme's adoption barrier for individual developers.
5. **Hook-based auto-updates** — PostEdit and PostGit hooks that automatically keep the graph current reduce developer friction.

## Bottom Line

**Claudette** is a focused, lightweight tool that solves one problem well: giving
Claude Code structural awareness of a codebase with minimal setup. It is a
developer convenience tool.

**Aethyme** is a platform-grade code intelligence system that goes beyond graph
construction — providing deterministic navigation, task-scoped context assembly,
risk classification, code health scoring, and multi-tenant API delivery. It is
infrastructure for AI agents at scale.

They share the same foundational insight (AI agents need structural code graphs),
but Aethyme's scope is an order of magnitude larger. Claudette's strength is its
simplicity and zero-friction local experience — something Aethyme could learn from
for its local developer story.
