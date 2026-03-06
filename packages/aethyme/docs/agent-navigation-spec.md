# Aethyme Core: Layered Agent Navigation Spec

Last Updated: 2026-03-06

## Purpose
This document is the canonical product and technical specification for Aethyme Core.

It defines:
- what Aethyme Core is trying to achieve
- the layered architecture of the product
- the phased implementation order
- the runtime and language split
- the primary product objects and evaluation model

This document is intentionally comprehensive. Shorter operational docs should point back to this one.

## Executive Thesis
Aethyme Core exists to be the primary repository navigation layer for AI coding agents.

Its job is to make repository work:
- more deterministic
- more context-efficient
- lower-retry
- easier to review
- increasingly scope-aware
- eventually permission-aware

The product is not a generic code search system and not a generic agent platform.
The product is a deterministic substrate for agent navigation, context construction, scope shaping, and later agent performance management.

## Product Goal
The first real goal is not to solve every agent problem.
It is to make single-task code changes meaningfully better.

Aethyme should help agents:
1. find the right starting point
2. gather the smallest useful context
3. understand dependency and impact boundaries
4. avoid irrelevant or high-risk areas unless needed
5. operate in a smaller, more reviewable scope

## What Aethyme Core Is
Aethyme Core is:
- a repository mapping engine
- a discoverability layer over that map
- a context-pack builder for tasks
- a guidance layer for agent navigation
- later, a policy-aware execution substrate
- later, a foundation for deterministic agent management and performance management

## What Aethyme Core Is Not
Aethyme Core is not:
- a customer-facing identity product
- a generic SaaS shell
- a general-purpose observability platform
- a vague AI platform without a hard substrate
- a broad agent orchestration system in v1

## Product Boundary
### `packages/aethyme`
Owns:
- repository intake
- repository mapping
- discoverability and traversal
- context-pack construction
- scope and risk inference
- advisory guidance
- scorecard and repo quality signals
- local autofix/operator tooling
- auth enforcement and tenant isolation
- API and CLI adapters over the core engine

### `packages/aethyme-cloud`
Owns:
- identity issuance
- user, org, tenant, and admin lifecycle
- SaaS UX and management surfaces
- later agent management and policy UX
- later performance reporting UI

## Canonical Model
The canonical business hierarchy is:
- Platform
- Org
- Tenant
- Repository
- Graph

The canonical product hierarchy is:
- Repo Intake
- Mapping
- Discoverability
- Context
- Guidance
- Policy
- Action Support
- Evaluation
- Agent Management

The business hierarchy describes ownership and isolation.
The product hierarchy describes capabilities and implementation order.

## Design Principles
1. Deterministic before clever.
2. Minimal context before exhaustive context.
3. Repository facts before policy overlays.
4. Advisory guidance before hard enforcement.
5. Understanding before action.
6. Action support before management abstractions.
7. Measured improvement before product expansion.

## What "Deterministic" Means
For Aethyme, deterministic means:
- the same repo state and same task should produce the same or near-identical navigation object
- retrieval order should be stable
- scope decisions should be explainable
- pack construction should not rely on probabilistic generation in v1
- agents should need fewer open-ended searches and retries

## What "Efficient" Means
For Aethyme, efficiency is primarily about:
- fewer retries
- lower token usage
- lower human review burden

Secondary efficiency metrics include:
- lower wall-clock time
- fewer unnecessary file reads
- fewer irrelevant tool calls

## Primary Users
Primary users, in order, are:
1. coding agents
2. review agents
3. orchestration agents

The first product target is coding agents doing single-task code changes.

## Main Failure To Eliminate First
The first failure Aethyme should reduce is:
- agents pulling too much irrelevant context

That is the highest-leverage problem because it drives:
- token waste
- retries
- poor scope control
- broader diffs
- harder reviews

## Layered Architecture
Aethyme should be implemented as a layered system with clear outputs between layers.

| Layer | Goal | Main output | Primary owner |
|---|---|---|---|
| 0. Repo Intake | Normalize a repository | repo identity, snapshot, metadata | Python |
| 1. Mapping | Build structural knowledge | files, symbols, edges, metadata | Rust |
| 2. Discoverability | Make structure queryable | lookup, search, neighbors, impact | Rust |
| 3. Context | Build minimal task packs | anchors, snippets, frontier, scope | Rust |
| 4. Guidance | Shape agent navigation | read order, warnings, out-of-scope | Rust |
| 5. Policy | Apply advisory rules | risk overlays, escalation hints | Python first |
| 6. Action Support | Help bounded code changes | edit scope, blast radius, review pack | Mixed |
| 7. Evaluation | Measure product value | token, retry, review metrics | Python |
| 8. Agent Management | Control agent behavior at scale | profiles, permissions, routing | Cloud later |

### Why Layers
This layering separates four different concerns that must not collapse into one implementation:
- repository facts
- discoverability and navigation
- behavior shaping
- policy and management

Without this separation, the system will become hard to reason about and hard to evolve.

## Layer 0: Repo Intake
### Purpose
Turn a repository into a stable unit Aethyme can reason about.

### Owns
- repo registration
- revision identity
- local snapshot identity
- language detection
- config discovery
- ignore rules
- basic repo metadata

### Outputs
- repo id
- revision id
- language set
- path inventory
- config inventory

### Rules
This layer prepares the repo for mapping. It does not yet interpret tasks.

## Layer 1: Mapping
### Purpose
Produce deterministic structural facts about the repository.

### Owns
- file inventory
- symbol extraction
- imports and includes
- containment relationships
- call and reference edges where trustworthy
- structural metadata
- basic risk tagging from deterministic rules
- confidence labeling on items and edges

### Outputs
- file graph
- symbol graph
- edge set
- metadata set
- risk annotations
- confidence annotations

### Rules
This layer should produce facts, not task decisions.
Policy does not belong here.

## Layer 2: Discoverability
### Purpose
Make the mapping layer queryable and navigable.

### Owns
- symbol lookup
- file lookup
- path lookup
- fuzzy search
- dependency neighborhood expansion
- impact neighborhood expansion
- anchor candidate retrieval
- deterministic ranking of candidates

### Outputs
- ranked candidates
- dependency frontier
- impact frontier
- neighborhood sets
- anchor candidates

### Rules
This layer returns candidates and graph-adjacent results.
It still does not build task-context packs.

## Layer 3: Context
### Purpose
Turn a task into a bounded working set.

### Owns
- task normalization
- task classification
- anchor resolution
- neighborhood expansion for the task
- pruning
- snippet selection
- initial scope assembly

### Main Output
The main output of this layer is the task-context pack.

### Why This Layer Matters
This is the first layer where Aethyme becomes directly useful to agents.
Aethyme is not just a graph if it can emit a bounded navigation object for a task.

## Layer 4: Guidance
### Purpose
Shape how the agent should navigate and operate.

### Owns
- recommended reading order
- recommended expansion order
- out-of-scope explanation
- risk-aware warnings
- likely edit scope
- likely no-touch zones

### Outputs
- navigation guidance
- scope guidance
- review guidance

### Rules
This is advisory but real. It should materially affect how an agent proceeds.
It is not yet enforcement.

## Layer 5: Policy
### Purpose
Overlay human and organizational control on top of facts and guidance.

### Owns
- manually defined high-risk areas
- advisory restrictions
- escalation-required zones
- later agent-profile overlays
- later fine-grained permissions

### Outputs
- policy overlays attached to packs and guidance

### Rules
Policy must stay separate from repository facts.
It is an overlay, not a replacement for mapping or discoverability.

## Layer 6: Action Support
### Purpose
Help agents make bounded, reviewable changes.

### Owns
- edit-scope suggestions
- blast-radius analysis
- pre-change checks
- post-change impact pack
- review-pack generation
- later constrained autofix support

### Outputs
- suggested edit set
- expected impact
- review-oriented context

### Rules
Action support is downstream of understanding.
Aethyme should not optimize for autonomous action before it can reliably shape navigation and scope.

## Layer 7: Evaluation
### Purpose
Prove that Aethyme improves agent work.

### Owns
- benchmark repos
- benchmark tasks
- control prompt
- with-vs-without-Aethyme runs
- token metrics
- retry metrics
- review burden metrics
- time metrics

### Outputs
- evaluation reports
- regression signals
- product truth

### Rules
The benchmark protocol is part of the product, not an afterthought.
Without it, Aethyme becomes hard to validate and easy to overclaim.

## Layer 8: Agent Management
### Purpose
Run a deterministic management layer on top of Aethyme.

### Owns
- agent profiles
- policy-linked execution modes
- performance tracking per agent
- later governance and permissions

### Outputs
- managed agent configurations
- performance reports
- policy-aware execution modes

### Rules
This is a later layer. It depends on the earlier layers being real.

## Phased Implementation Model
Aethyme should be built in phases that follow the layers.

| Phase | Goal | Main layers |
|---|---|---|
| 1. Foundation Mapping | build a trustworthy substrate | 0, 1 |
| 2. Discoverability | make the substrate usable | 2 |
| 3. Context Packs v1 | emit bounded task packs | 3 |
| 4. Guidance v1 | shape agent navigation | 4 |
| 5. Evaluation Harness | prove product value | 7 |
| 6. Action Support | constrain code changes | 6 |
| 7. Policy v1 | add advisory control | 5 |
| 8. Managed Agent Layer | support deterministic agent management | 8 |

### Phase 1: Foundation Mapping
#### Goal
Build a trustworthy structural substrate.

#### Includes
- repo intake
- file and symbol graph
- deterministic edge model
- basic risk taxonomy
- confidence model

#### Exit Criteria
- repos can be indexed deterministically
- symbols, files, and edges are queryable
- high-risk path tagging works
- mapping quality is measurable

### Phase 2: Discoverability
#### Goal
Make the repository map usable as a navigation backend.

#### Includes
- symbol, file, and path lookup
- dependency frontier
- impact frontier
- anchor candidate ranking
- deterministic ordering

#### Exit Criteria
- the same query returns stable results
- dependency and impact queries are usable
- search quality is good enough to seed tasks

### Phase 3: Context Packs v1
#### Goal
Produce minimal, non-LLM task-context packs.

#### Includes
- task taxonomy
- task normalization
- anchor resolution
- pack schema
- pruning rules
- snippet selection

#### Exit Criteria
- one task in, one stable pack out
- packs are small enough to be useful
- the same task and repo state yield near-identical packs

### Phase 4: Guidance v1
#### Goal
Start shaping agent navigation behavior.

#### Includes
- navigation order
- in-scope list
- out-of-scope list
- high-risk warnings
- likely no-touch zones

#### Exit Criteria
- packs tell agents what to read first
- packs tell agents what to avoid
- risky areas are surfaced clearly

### Phase 5: Evaluation Harness
#### Goal
Measure whether Aethyme actually improves agent work.

#### Includes
- control prompt
- with-vs-without-Aethyme benchmark harness
- token measurement
- retry measurement
- review burden measurement
- repository explanation benchmark
- small code-change benchmark
- later CTF-style benchmark tasks

#### Exit Criteria
- repeatable benchmarks exist
- Aethyme can be compared against baseline
- token and retry improvements are observable

### Phase 6: Action Support
#### Goal
Support bounded code changes, not just navigation.

#### Includes
- edit-scope suggestions
- blast-radius hints
- post-change impact pack
- review pack
- safer autofix integration

#### Exit Criteria
- Aethyme helps constrain changes
- reviewability improves
- scope creep becomes measurable

### Phase 7: Policy v1
#### Goal
Add advisory control over repository areas.

#### Includes
- user-defined high-risk areas
- policy overlay model
- escalation-required annotations
- advisory blocked zones

#### Exit Criteria
- policy changes pack and guidance output
- manually defined high-risk areas work
- advisory controls are explainable

### Phase 8: Managed Agent Layer
#### Goal
Turn Aethyme into a deterministic agent control plane.

#### Includes
- agent profiles
- policy-linked execution modes
- performance management
- governance surface
- later enforceable permissions

#### Exit Criteria
- agents can run under defined navigation and policy modes
- performance can be compared per profile
- management is real, not conceptual

## First-Class Product Unit
The first-class external product unit is the task-context pack.

Internally, Aethyme works with:
- files
- symbols
- edges
- risk annotations
- confidence annotations

Externally, it should emit:
- a bounded task-context pack

The pack is the primary navigation object for an agent.
It should replace broad repository wandering with a smaller, deterministic working set.

## Context Pack v1
### Non-LLM First
The first version of the context pack should be fully deterministic.

Aethyme itself should emit the pack structure without requiring an LLM.
LLM-generated summaries may be added later, but they should not define pack membership, scope, or risk.

### Role
The context pack is:
- a bounded working set
- a navigation control surface
- a scope-shaping object
- an evidence bundle for the task

### Recommended Fields
- `task`
- `goal`
- `anchors`
- `summary`
- `snippets`
- `dependencies`
- `impact`
- `in_scope`
- `out_of_scope`
- `risk_flags`
- `navigation_order`
- `confidence`
- `budget`

### Output Style
Default output should include both:
- structured machine-readable content
- raw supporting evidence

The summary is the navigation layer.
The snippets are the proof layer.

## Task Model
The first task categories should be explicit.

Recommended v1 task kinds:
- `explain_repo`
- `explain_component`
- `change_symbol`
- `trace_impact`

The first product focus is single-task code changes.
Repository explanation is still important because it is a clean navigation benchmark.

## Task-To-Anchor Resolution
Aethyme should resolve anchors deterministically.

### Resolution Cascade
1. explicit mentions
2. keyword extraction
3. candidate lookup
4. anchor scoring
5. anchor selection

### Selection Rules
- prefer symbols over files when a symbol is reliable
- prefer files over folders when a file is specific enough
- prefer narrow anchors over broad central anchors
- include the reason each anchor was selected

## Scope Model
The first version should distinguish between different kinds of scope.

At minimum, Aethyme should reason about:
- likely relevant scope
- likely edit scope
- likely impact scope
- likely out-of-scope areas

This should not collapse into a single label.

## Out-Of-Scope Model
Out-of-scope should be treated as a real feature.

Out-of-scope can mean:
- not relevant to the task
- too far from the current dependency frontier
- high-risk unless explicitly needed
- low-confidence relation
- policy-restricted later

High-risk areas are especially important in this category.

## Risk Model
### Purpose
Risk exists to help the agent avoid costly or dangerous expansion.

### v1 Taxonomy
Recommended initial taxonomy:
- auth
- permissions
- secrets
- migrations
- infra
- billing
- shared-core
- destructive paths

### Detection Sources
Risk can come from:
- path rules
- naming rules
- graph position later
- user-defined overlays later

### Long-Term Direction
Users should eventually be able to define their own high-risk areas.
That capability belongs in the policy layer, not the mapping layer.

## Policy Direction
### Short Term
Policy is advisory first.
It should influence context packs and guidance, but not yet hard-block execution.

### Long Term
Policy should support:
- user-defined high-risk areas
- escalation-required zones
- blocked zones
- role or profile-based rights
- later file, folder, and function-level controls

## Evaluation Model
### Core Principle
Aethyme should be evaluated against a control condition.

The right question is not only whether a query looks good.
The real question is whether agents do better work with Aethyme than without it.

### Primary Metrics
Primary metrics are:
- token usage
- retry count
- human review burden

Secondary metrics include:
- wall-clock time
- number of files touched
- percent of changes outside expected scope

### First Benchmark
The first benchmark should be a structured repository explanation task.
This is a clean way to measure navigation quality, token usage, and time.

### Next Benchmark
The next benchmark should be a small, fixed code-change task.
That is closer to the actual product goal.

### Future Benchmark
A CTF-style benchmark suite is a strong long-term direction.
It provides fixed goals, measurable success, and harder-to-game evaluations.

## Language And Runtime Direction
Aethyme should move as much deterministic engine logic to Rust as is reasonable.
Python should remain the delivery and orchestration layer.

### Rust First
Rust should own, over time:
- mapping kernels
- discoverability kernels
- context-pack types and assembly
- scope and risk inference kernels
- graph expansion and pruning kernels
- later policy evaluation where determinism and performance matter

### Python Retained
Python should keep, for now:
- FastAPI delivery layer
- CLI layer
- auth enforcement layer
- scorecard orchestration
- SDKs
- migrations and operational wiring
- evaluation harnesses
- product orchestration around the Rust engine

## Recommended v1 Product Scope
The first meaningful Aethyme product should include only:
1. Repo Intake
2. Mapping
3. Discoverability
4. Context Packs v1
5. Guidance v1
6. Evaluation Harness

It should not yet try to fully deliver:
- policy enforcement
- broad automation
- managed agent platform features

## Near-Term Technical Priorities
1. define the Rust engine boundary clearly
2. implement mapping and discoverability kernels in Rust
3. implement task-context pack assembly in Rust
4. expose thin Python adapters over the Rust engine
5. build the evaluation harness in Python
6. improve graph quality and confidence labeling underneath the pack layer

## Near-Term Product Priorities
1. reduce irrelevant context for single-task code changes
2. produce stable task-context packs
3. surface high-risk and out-of-scope areas clearly
4. prove token and retry improvement against a control condition
5. keep the system honest and small while the substrate solidifies

## Long-Term Outcome
If successful, Aethyme Core becomes:
- the primary interface between AI coding agents and repositories
- a deterministic navigation substrate
- a scope and risk shaping system
- later, a policy-aware execution substrate
- later, a deterministic agent management and performance management layer
