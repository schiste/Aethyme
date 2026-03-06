# Aethyme Core Vision

Last Updated: 2026-03-06

## Core Thesis

Aethyme Core exists to make AI code changes as deterministic, efficient, and
reviewable as possible.

It should become the primary interface between AI coding agents and a
repository.

The goal is not to give agents occasional help.

The goal is for agents to navigate through Aethyme first, use raw repository
access less and less over time, and eventually operate inside explicit scope
and permission boundaries.

## The Problem

AI agents waste effort on repositories because they:

- pull too much irrelevant context
- read too many files before acting
- retry too often
- spend too many tokens on weak retrieval
- produce changes that are harder for humans to review

Browsing a repository like a human with raw file search is not a good default
for agents.

Agents need:

- a deterministic navigation layer
- minimal task-scoped context
- reliable dependency and impact understanding
- clear scope boundaries

That is the job of Aethyme Core.

## What We Are Building

Aethyme Core is the backend navigation and execution substrate for AI coding
agents.

It should:

1. turn repositories into structured graphs
2. identify the right starting points for a task
3. return the smallest useful context pack
4. show dependency and impact boundaries
5. tell the agent what is likely in scope
6. tell the agent what is likely out of scope
7. later, attach advisory and then enforceable permissions to repository areas

The graph is the substrate.

The real product value is deterministic navigation, bounded context, and
increasingly explicit scope control.

## First Product Focus

Optimize for single-task code changes first.

That means Aethyme should help an agent:

1. locate the right symbols and files
2. understand what they depend on
3. understand what they affect
4. avoid irrelevant repository areas
5. keep the change set small and reviewable

The first proof of value is not broad platform breadth.

It is fewer retries, fewer tokens, and lower human review burden on a single
coding task.

## First-Class Product Unit

The first-class output should be a task-context pack.

Internally that pack is built from:

- symbol anchors
- dependency edges
- impact edges
- file evidence

But externally the thing that matters is:

- a minimal structured summary
- raw supporting snippets
- likely in-scope files and symbols
- likely out-of-scope files and symbols

## What Core Owns

Aethyme Core should own:

- repository indexing
- graph construction
- deterministic retrieval primitives
- task-context construction
- dependency and impact navigation
- repository quality scoring
- local autofix support
- scoped enforcement of org and tenant boundaries
- a Rust-first deterministic engine layer for the parts of the product where performance and explicitness matter most

`packages/aethyme-cloud` should wrap this with SaaS concerns.

## What Core Is Not

Aethyme Core is not:

- a customer identity product
- a generic SaaS admin system
- a vague “AI platform”
- a broad orchestration layer with weak substrate
- a human-first code browsing product

Its purpose is much narrower and more important:

make agent work on repositories less wasteful and less probabilistic.

## Product Pillars

### 1. Graph Substrate

The repository must be modeled as symbols, dependencies, and impact paths.

### 2. Minimal Context Packs

Agents should get the smallest useful task-scoped context by default.

### 3. Scope Intelligence

Aethyme should indicate what is likely relevant and what is likely out of scope.

### 4. Repository Quality Signals

Scorecard should measure repository quality first, while also informing how
well agents can operate on the codebase.

### 5. Policy And Permission Control

This starts advisory.

Later it should support real controls such as:

- escalation-required areas
- blocked areas
- file-level restrictions
- folder-level restrictions
- function-level restrictions
- RBAC-like agent permissions

### 6. Agent Management And Performance

Long term, Aethyme should support a deterministic agent management and
performance layer built on top of this substrate.

That means measuring and improving:

- token efficiency
- retry rates
- scope discipline
- reviewability
- consistency of agent behavior

## Design Principles

### 1. Navigation First

If agents still navigate mainly through raw repo access, Aethyme has not become
the real substrate yet.

### 2. Minimal By Default

Context should be bounded and aggressively pruned by default.

### 3. Determinism Over Cleverness

Stable retrieval and stable scope are more valuable than flashy but inconsistent
behavior.

### 4. Advisory Before Enforcement

Scope and permission rules should begin as guidance, then mature into explicit
control.

### 5. Honest Surface Area

Only tested, active behavior should be treated as product reality.

## Near-Term Mission

In the near term, success means:

1. establish the Rust engine boundary
2. improve fallback indexing and graph quality
3. build strong task-context packs
4. reduce irrelevant context for coding tasks
5. improve dependency and impact trustworthiness
6. start expressing in-scope and out-of-scope areas explicitly

## Long-Term Position

If Aethyme Core succeeds, it becomes:

- the default repository navigation layer for agents
- the substrate for deterministic agent execution
- the base for agent performance management
- the base for permission-aware AI change systems

That is the real vision.

Not just better search.

Not just a repo graph.

A system that makes AI work on codebases materially more controlled and
predictable.
