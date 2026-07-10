# Aethyme Core Vision

Last Updated: 2026-03-08

> **Status (2026-07-09): aspirational document — read with care.**
> The mechanics this document treats as the product — Lenses, Learning /
> weight adjustment, Live Session trajectory tracking, enriched non-raw-file
> responses, the replay engine — have **no implementing code** in this
> repository. What exists today is the deterministic graph engine and CLI
> ("the paper map"). Product direction has since shifted toward a
> **local-first agent broker** (see
> [`docs/aethyme-local-agent-broker.md`](../../../docs/aethyme-local-agent-broker.md)
> at the repository root), which supersedes parts of this vision. This file is
> retained as historical thinking, not as a roadmap.

## Core Thesis

Aethyme Core is a GPS for AI coding agents navigating repositories.

It should become the primary interface between AI coding agents and a
repository. The goal is not to give agents occasional help. The goal is for
agents to navigate through Aethyme first, use raw repository access less and
less over time, and eventually operate inside explicit scope and permission
boundaries.

A paper map shows you the roads. A GPS knows the roads, knows where you are,
knows where you are going, knows which roads are fast, and reroutes when you
drift. Aethyme is the GPS.

## The Problem

AI agents waste effort on repositories because they:

- pull too much irrelevant context
- read too many files before acting
- retry too often
- spend too many tokens on weak retrieval
- produce changes that are harder for humans to review
- use tools without supply chain accountability
- have no feedback loop to improve over time

Browsing a repository like a human with raw file search is not a good default
for agents. It is the equivalent of navigating a city without a map.

Agents need:

- a deterministic navigation layer
- minimal task-scoped context
- reliable dependency and impact understanding
- clear scope boundaries
- contextually appropriate tooling
- a system that gets smarter from their work, not just from code analysis

That is the job of Aethyme Core.

## Mental Model: GPS, Not Paper Map

The difference between Aethyme and a static code index is the difference
between a GPS and a printed map.

A GPS does five things a map cannot:

1. **It knows where you are right now.** Not where you started. Where you are.
2. **It adapts to your trip.** A delivery route highlights different roads than
   a commute.
3. **It learns from traffic.** Millions of trips make the routing smarter
   without changing the road network.
4. **It narrows as you get closer.** Highway-level overview becomes
   street-level detail.
5. **It reroutes.** You took a wrong turn? It recalculates.

Aethyme should do all five for agent repository work.

## Five Core Concepts

### 1. The Map

A deterministic graph built from repository structure: files, symbols, edges,
areas, risk zones, documentation links. Built in Rust. Indexed once per repo
state. Same repo state produces the same map.

The map is structural truth. It does not encode opinions, task decisions, or
policy. It answers: what exists, where it lives, what it depends on, what
depends on it, what documents it, what configures it, and what is risky.

### 2. Lenses

Named, described navigation profiles that reweight the map based on task type.
Aethyme offers available lenses. The agent picks one. The choice is logged and
reviewable.

Each lens controls:

- what graph edges are bright, dim, or invisible
- what traversal strategy is used (depth-first, breadth-first, impact-radiating)
- what node types are promoted or suppressed
- what tools from the registry are relevant and promoted

Example lenses:

- `debug` — call chains bright, error paths promoted, tests promoted, docs dim
  unless risk is high
- `feature` — architecture docs bright, similar patterns highlighted, deep
  internals dim
- `refactor` — impact edges bright, type boundaries promoted, dependency depth
  prioritized
- `security` — risk zones bright, auth paths promoted, everything else heavily
  dimmed
- `explain` — docs and structure bright, code details dim, overview-first
  traversal

Lenses have designed defaults but self-calibrate per repository over time based
on execution data. A repo where debug tasks always require documentation will
see the `debug` lens gradually promote docs for that specific repo.

### 3. Enriched Responses

The agent queries the map through its active lens. Aethyme does not return raw
files. It returns contextually shaped neighborhoods. The guidance is embedded
in the response, not layered on top.

The same query with the same file returns different responses depending on:

- the active lens
- the agent's trajectory so far in this session
- the risk profile of the area being accessed
- the learned weights from historical tasks on this repo

Response depth is graduated, not binary:

- **Silent shaping** — weights shape which edges the agent sees at all
- **Enriched** — contextually important extras are attached without being asked
- **Annotated** — risk flags, warnings, scope notes included inline
- **Redirected** — "what you want is actually over here"

Most of the time it is the first two. The agent does not realize it is being
guided. It just makes better decisions because the map is shaped for its task.

### 4. Live Session

Chau7 streams agent behavior to Aethyme in real time: which files were read,
which tools were used, tokens spent, trajectory through the graph. Aethyme
maintains a live navigation state per active task.

The map does not just answer queries. It tracks where the agent is and adjusts
responses based on trajectory. If the agent drifts from the relevant subgraph,
subsequent responses silently steer it back by promoting closer, more productive
paths and dimming the area it wandered into.

This is not a separate intervention channel. The guidance is always embedded in
the navigation responses themselves.

### 5. Learning

After every task, execution telemetry flows back into the map as weight
adjustments. Over time, each repository develops its own navigation fingerprint.

Signals accumulate on nodes:

- anchor quality per task type
- token cost (average tokens consumed when this file enters context)
- danger signal (how often changes here fail CI or get rejected in review)
- dead end signal (how often agents read this and it turns out irrelevant)

Signals accumulate on edges:

- traversal success per task type
- traversal cost
- shortcut signal (this edge skips a common multi-hop path)

Signals accumulate on areas:

- drift zone (agents enter but it is rarely part of successful changes)
- hotspot (high change frequency, high success variance)
- stable zone (rarely touched, changes usually succeed)

Lenses do not just have designed weights. They have learned weights. The `debug`
lens promotes the specific call chains that historically lead to fast resolution
in this repo. Every repo develops its own navigation personality.

Recent data matters more than old data. The codebase changes. Signals decay.

## What We Are Building

Aethyme Core is the navigation intelligence layer for AI coding agents.

It should:

1. turn repositories into deterministic structural graphs
2. offer task-typed lenses that shape how the graph is navigated
3. return enriched, contextually shaped responses (not raw files)
4. track agent trajectory in real time via Chau7 telemetry
5. learn from execution outcomes to make the map smarter over time
6. tell the agent what is likely in scope and what is not
7. scope which tools are relevant and promoted for a given task
8. attach advisory and later enforceable permissions to repository areas

The graph is the substrate. Lenses are the interface. Learning is the flywheel.

## First Product Focus

Optimize for single-task code changes first.

That means Aethyme should help an agent:

1. locate the right symbols and files
2. understand what they depend on
3. understand what they affect
4. avoid irrelevant repository areas
5. keep the change set small and reviewable
6. use the right tools for the task type

The first proof of value is fewer retries, fewer tokens, and lower human review
burden on a single coding task.

## Ecosystem Integration

Aethyme is one part of a four-product ecosystem. Each product is open core and
independently useful. Together they form a closed feedback loop.

### Chau7 (AI-native terminal)

The sensor network. Chau7 observes everything agents do: tool calls, file reads,
tokens spent, task boundaries, CI outcomes, git changes. It streams this
telemetry to Aethyme in real time and captures full sessions for replay.

The agent does not need to report anything. Chau7 watches transparently.

### Registry (skills and MCP curation)

The tool supply chain. An open source registry of skills and MCP servers with
security auditing and trust scoring. Agents query the registry to find and
install tools as needed, but only from trusted sources.

The registry also accumulates effectiveness data: which tools work well for
which task types, based on real execution outcomes flowing back from Chau7.

Aethyme lenses scope which registry tools are relevant and promoted for a
given task type.

### Aeptus (security posture management)

The governance layer. Aeptus manages organizational security posture: suppliers,
assets, attack surface, controls, compliance.

AI agents operating on repositories are assets on the organization's attack
surface. Aethyme's risk data, behavioral audit trails, and scope compliance
signals feed into Aeptus as an agent posture module: which agents accessed
what, with what tools, within what policy boundaries.

### The Feedback Loop

```
Agent gets task
       |
       v
  Aethyme --- lens + enriched nav ---> Agent works
  (GPS)                                    |
       ^                                   |
       |                              Chau7 observes
       |                              (sensor)
       |                                   |
       +--- execution signals flow back ---+
       |
       +--- Registry: tool effectiveness updates
       +--- Aeptus: agent posture data
```

## Replay And Evaluation

Because Chau7 captures full sessions, Aethyme provides deterministic maps, and
the registry controls the toolchain, the entire agent execution is reproducible.

The replay engine can isolate variables:

- **Model**: same repo, same map, same tools. Which model performs best?
- **Map version**: same model, same repo. Did the learned weights improve
  outcomes?
- **Tool set**: same model, same map. Did adding or removing a tool change
  results?
- **No Aethyme vs with Aethyme**: the fundamental product value proof.

Evaluation is not a separate harness. Every real task is an evaluation. Did the
agent drift? How many interventions were needed? Did it follow the guidance? Did
the task succeed? That runs continuously on real work.

For enterprise customers, nightly replay can re-run selected real tasks against
different models and map versions, producing hard metrics: tokens, time, CI
outcomes, scope drift, files touched.

## Open Core Strategy

Aethyme is open core.

| Free / Open | Paid / Intelligence Layer |
|---|---|
| Rust graph engine | Learning loop (weight updates from telemetry) |
| Repo indexing | Live session tracking |
| Static lens definitions | Lens self-calibration per repo |
| CLI navigation | Replay engine |
| Basic MCP server | Enriched responses with learned weights |
| Context pack assembly | Cross-product analytics |

The free version is a paper map: deterministic, useful, static.

The paid version is the GPS: it learns, adapts, tracks trajectory, and gets
smarter every day.

The moat is not the algorithm. The moat is accumulated execution intelligence
on a specific codebase. The longer a team uses it, the smarter it gets, and
that learning cannot be replicated without the same execution history.

## Product Pillars

### 1. Graph Substrate

The repository modeled as symbols, dependencies, and impact paths.

### 2. Lenses

Task-typed navigation profiles that reweight the graph and scope tooling.

### 3. Enriched Navigation

Contextually shaped responses that embed guidance invisibly.

### 4. Live Trajectory

Real-time session tracking via Chau7 telemetry.

### 5. Learning

Execution data feeding back into map weights, lens calibration, and tool
effectiveness scores.

### 6. Scope Intelligence

In-scope, out-of-scope, and risk signals expressed clearly in every response.

### 7. Policy And Permission Control

Starting advisory. Later supporting escalation-required areas, blocked zones,
file-level and function-level restrictions, and role-based agent permissions.

### 8. Replay And Evaluation

Reproducible agent execution for model grading, map regression testing, and
continuous product value proof.

## Design Principles

### 1. GPS, Not Paper Map

If the system does not adapt to the agent's trajectory and task type, it is
just a better index. The goal is live, contextual navigation.

### 2. Navigation First

If agents still navigate mainly through raw repo access, Aethyme has not become
the real substrate yet.

### 3. Invisible Guidance

The best guidance is embedded in the response, not layered on top. The agent
should make better decisions without realizing it is being steered.

### 4. Minimal By Default

Context should be bounded and aggressively pruned by default.

### 5. Determinism Over Cleverness

Stable retrieval and stable scope are more valuable than flashy but
inconsistent behavior.

### 6. Advisory Before Enforcement

Scope and permission rules begin as guidance, then mature into explicit control.

### 7. Honest Surface Area

Only tested, active behavior should be treated as product reality.

### 8. Never Overfit To Evals

The tools, engine, and pipeline must never be modified to improve eval scores.
Evals are diagnostics, not targets. If an eval reveals a weakness, fix the
generic system. If the fix only makes sense in the context of a specific eval
metric, it is overfitting and must not ship. A system that scores 70 honestly
is worth more than one that scores 95 through accommodation. See the eval
protocol for the full rule.

### 9. Learn From Traffic

The system should get smarter from real agent work, not just from code analysis.
Execution data is the highest-value signal.

## Near-Term Mission

In the near term, success means:

1. establish the Rust engine boundary
2. improve fallback indexing and graph quality
3. define and implement the lens model
4. build enriched response assembly
5. design the Chau7 telemetry event stream
6. build the MCP surface agents query for navigation
7. reduce irrelevant context for coding tasks
8. start expressing in-scope and out-of-scope areas explicitly

## Long-Term Position

If Aethyme Core succeeds, it becomes:

- the default repository navigation layer for agents
- a live, learning navigation system that improves with every task
- the substrate for deterministic agent execution
- the base for agent performance management
- the base for permission-aware AI change systems
- part of a four-product ecosystem that provides full observability, governance,
  and tool supply chain management for AI agent work

Not just better search. Not just a repo graph.

A GPS that makes AI work on codebases materially more controlled, predictable,
and continuously improving.
