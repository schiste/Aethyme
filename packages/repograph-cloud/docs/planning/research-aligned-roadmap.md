# RepoGraph Cloud - Research-Aligned Roadmap
**Created:** October 6, 2025
**Based On:** [repograph-research.md](../../repograph-research.md)
**Current Status:** 10-15% of research vision implemented

---

## 🎯 Executive Summary

### The Gap

**Research Vision (from repograph-research.md):**
- Advanced graph theory + neurobiology-inspired code intelligence
- Event-Segmented Code Memory (ESCM)
- Hierarchical Code Memory Architecture (HCMA)
- Prediction-Error-Driven Graph Retrieval (PEDGR)
- Cognitive Workspace for Code Agents (CWCA)
- Multimodal Community-Aware Graph Memory (MCGM)

**Current Implementation (packages/repograph-cloud/):**
- Basic infrastructure (PostgreSQL, Redis, Elasticsearch)
- Some backend code written but API broken
- NO graph features implemented
- NO AI-powered features working
- **10-15% of research vision** at best

### What This Means

The research document describes a **world-class, novel code intelligence platform** based on cutting-edge research. The current implementation is a **basic search prototype** that's not even working.

**We need to decide:**
1. Build toward the research vision (6-12 months)
2. Ship a simpler product first, evolve later (3-4 weeks)
3. Restart with research-first architecture (8-12 weeks)

---

## 📊 Current State vs. Research Vision

### What's Actually Working

| Component | Research Spec | Current State | Gap |
|-----------|---------------|---------------|-----|
| **Infrastructure** | PostgreSQL + Redis + Elasticsearch | ✅ Running | 0% gap |
| **Multi-tenant RLS** | Row-level security | ✅ Configured | 0% gap |
| **API Framework** | FastAPI + OpenAPI | ⚠️ Exists but broken | 50% gap |

### What's Partially Built

| Feature | Research Spec | Current Code | Working? | Gap |
|---------|---------------|--------------|----------|-----|
| **Ego Graph** | Typed edges, direction filters, pagination | Basic recursive CTE | ❌ API down | 70% gap |
| **Impact Analysis** | Bidirectional, path enumeration, time budgets | Reverse traversal only | ❌ API down | 80% gap |
| **Search** | Hybrid (exact/fuzzy/FTS) + semantic re-rank | Basic search code | ❌ API down | 60% gap |
| **Indexing** | SCIP-first + tree-sitter + incremental | SCIP + fallback | ❌ Untested | 50% gap |

### What's Completely Missing (0% Built)

| Research Feature | Priority | Complexity | Est. Effort |
|------------------|----------|------------|-------------|
| **Event-Segmented Code Memory (ESCM)** | 🔴 CRITICAL | Very High | 4-6 weeks |
| **Hierarchical Code Memory (HCMA)** | 🔴 CRITICAL | Very High | 4-6 weeks |
| **Prediction-Error-Driven Retrieval (PEDGR)** | 🔴 CRITICAL | Very High | 3-5 weeks |
| **Cognitive Workspace (CWCA)** | 🔴 CRITICAL | Very High | 4-6 weeks |
| **Community Detection** | 🟠 HIGH | High | 3-4 weeks |
| **Multimodal Graph (docs/tests/issues)** | 🟠 HIGH | Medium | 2-3 weeks |
| **Semantic Search + pgvector** | 🟠 HIGH | Medium | 2 weeks |
| **Query-by-Example (QBE)** | 🟡 MEDIUM | Medium | 1-2 weeks |
| **Streaming APIs (SSE/WebSocket)** | 🟡 MEDIUM | Low | 1 week |
| **Saved Searches & Alerts** | 🟡 MEDIUM | Low | 1 week |

**Total Missing:** ~30-45 weeks of work (7-11 months for 1 engineer)

---

## 🤔 Strategic Decision Required

### Option A: Research-First Implementation (Recommended)

**Goal:** Build the system described in repograph-research.md properly from the start

**Timeline:** 6-12 months to first production release

**Approach:**
1. **Month 1-2:** Fix infrastructure + implement research-aligned graph database schema
2. **Month 3-4:** Build Event-Segmented Code Memory (ESCM) + basic graph queries
3. **Month 5-6:** Implement Hierarchical Code Memory Architecture (HCMA)
4. **Month 7-8:** Add Prediction-Error-Driven Retrieval (PEDGR)
5. **Month 9-10:** Build Cognitive Workspace (CWCA)
6. **Month 11-12:** Multimodal Community-Aware Graph Memory (MCGM) + polish

**Pros:**
- ✅ Build novel, differentiated product
- ✅ Implement cutting-edge research
- ✅ Create defensible moat (hard to copy)
- ✅ Attract top-tier talent and funding
- ✅ Proper architecture from day one

**Cons:**
- ❌ Long time to revenue (6-12 months)
- ❌ High technical risk
- ❌ Requires ML/AI expertise
- ❌ Complex to explain to customers initially

**Investment Required:**
- 2-3 senior engineers (full-time)
- 1 ML/AI specialist (full-time)
- 1 DevOps engineer (part-time)
- ~$150-200K for 6 months

---

### Option B: Simple MVP First, Research Later (Pragmatic)

**Goal:** Ship working product in 3-4 weeks, evolve toward research vision over time

**Timeline:** 3-4 weeks to shippable MVP, then iterative enhancement

**MVP Scope:**
1. Fix API server (Week 1)
2. Basic graph queries - ego + impact (Week 2)
3. Code search (no AI) (Week 3)
4. OAuth + repository indexing (Week 4)

**Then Iterate:**
- Month 2: Add semantic search
- Month 3: Add community detection
- Month 4-6: Begin ESCM implementation
- Month 7-12: Add HCMA, PEDGR, CWCA incrementally

**Pros:**
- ✅ Fast path to revenue/validation
- ✅ Learn from real users
- ✅ Lower initial risk
- ✅ Can pivot if needed

**Cons:**
- ❌ May accumulate technical debt
- ❌ Harder to retrofit research features later
- ❌ Less differentiated initially
- ❌ Risk of never reaching research vision

**Investment Required:**
- 1-2 engineers (full-time, 1 month)
- Then expand as revenue permits

---

### Option C: Hybrid - Research Architecture, Incremental Features

**Goal:** Build research-aligned architecture now, ship features incrementally

**Timeline:** 2-3 months to research-ready platform, then monthly feature releases

**Approach:**
1. **Month 1:** Redesign schema + APIs for research features (even if not implemented)
2. **Month 2:** Implement core graph queries + indexing (no AI yet)
3. **Month 3:** Add basic semantic search + ship MVP
4. **Month 4+:** Add research features one at a time (ESCM → HCMA → PEDGR → CWCA → MCGM)

**Pros:**
- ✅ Future-proof architecture
- ✅ Can ship simple features quickly
- ✅ Easier to add research features later
- ✅ Balanced risk/reward

**Cons:**
- ⚠️ Upfront architecture work delays initial ship
- ⚠️ May over-engineer for near-term needs
- ⚠️ Still requires sustained investment

**Investment Required:**
- 2 senior engineers (2-3 months)
- Then add ML specialist as needed

---

## 📋 Recommended: Hybrid Approach (Option C)

### Phase 0: Research-Aligned Foundation (Weeks 1-8)

**Goal:** Fix current code + build schema/APIs for future research features

#### Week 1-2: Fix & Stabilize
- [ ] Fix API server (critical blocker)
- [ ] Apply missing migrations
- [ ] Test basic endpoints work
- [ ] Get health checks passing
- [ ] Document what actually works

#### Week 3-4: Research-Aligned Schema
- [ ] Design schema for ESCM (event segmentation)
- [ ] Add tables for HCMA (memory hierarchy)
- [ ] Prepare for PEDGR (error-driven retrieval)
- [ ] Add CWCA workspace tables
- [ ] Implement community detection schema
- [ ] Add pgvector for embeddings

**Deliverable:** Working API + research-ready database schema

---

### Phase 1: Core Graph Intelligence (Weeks 5-12)

**Goal:** Implement research-specified graph features (without AI initially)

#### Weeks 5-6: Ego Graph v2
Following [repograph-research.md Feature Design #1](../../repograph-research.md):
- [ ] Typed edge traversal (INVOKE, IMPORT, INHERIT, CONTAIN, PARAMETER, RETURN)
- [ ] Directional filters (both/out/in)
- [ ] Per-depth shaping with fan-out caps
- [ ] Pagination support
- [ ] Response metadata (limits_applied, truncated_depths, provenance)
- [ ] Edge weighting and prioritization
- [ ] **Target:** p95 < 75ms for depth≤3

#### Weeks 7-8: Impact Analysis v2
Following [repograph-research.md Feature Design #2](../../repograph-research.md):
- [ ] Bidirectional traversal (forward/backward/both)
- [ ] Path enumeration with constraints
- [ ] Meet-in-the-middle BFS
- [ ] Time budget controls (partial results if exceeded)
- [ ] Sample path extraction
- [ ] **Target:** p95 < 200ms for 10 hops

#### Weeks 9-10: Call Graph Baseline
Following [repograph-research.md Feature Design #3](../../repograph-research.md):
- [ ] SCIP-first call graph extraction
- [ ] Tree-sitter fallback for call edges
- [ ] Provenance tracking (SCIP vs fallback)
- [ ] Confidence scores for inferred edges
- [ ] Data-flow edges (basic)

#### Weeks 11-12: Visualization & Streaming
Following [repograph-research.md Feature Design #9](../../repograph-research.md):
- [ ] SSE/WebSocket endpoints for graph streaming
- [ ] Per-depth batch emission
- [ ] Progress indicators
- [ ] Cancellation support
- [ ] **Target:** First visible result ≤ 150ms

**Deliverable:** Production-ready graph query engine

---

### Phase 2: Semantic Layer (Weeks 13-18)

**Goal:** Add AI-powered semantic capabilities

#### Weeks 13-14: Semantic Search
Following [repograph-research.md Feature Design #6](../../repograph-research.md):
- [ ] pgvector integration
- [ ] Embedding generation service (OpenAI/Claude/local)
- [ ] Hybrid search (lexical + semantic fusion)
- [ ] Re-ranking with configurable alpha
- [ ] **Target:** p95 < 120ms

#### Weeks 15-16: Query-by-Example (QBE)
Following [repograph-research.md Feature Design #7](../../repograph-research.md):
- [ ] Embedding similarity
- [ ] Structural similarity (edge histograms)
- [ ] Diversity re-ranking (MMR)
- [ ] **Target:** Precision@10 ≥ 0.70

#### Weeks 17-18: Community Detection
Following [repograph-research.md Feature Design (Community Layer)](../../repograph-research.md):
- [ ] Louvain/Leiden algorithm implementation
- [ ] Hierarchical community structure
- [ ] Community summaries (LLM-generated)
- [ ] Commit-keyed caching

**Deliverable:** AI-powered search + discovery

---

### Phase 3: Advanced Research Features (Weeks 19-32)

**Goal:** Implement the novel research contributions

#### Weeks 19-22: Event-Segmented Code Memory (ESCM)
Following [repograph-research.md Idea #1](../../repograph-research.md):
- [ ] Bayesian surprise detection for code events
- [ ] Graph-theoretic boundary refinement
- [ ] Event episodic memory storage
- [ ] Similarity-based + temporally contiguous retrieval
- [ ] **Innovation:** Brain-inspired code segmentation

#### Weeks 23-26: Hierarchical Code Memory (HCMA)
Following [repograph-research.md Idea #2](../../repograph-research.md):
- [ ] Sensory buffer (recent code)
- [ ] Short-term memory (compressed modules)
- [ ] Long-term graph memory (full repository)
- [ ] Central executive (attention control)
- [ ] Multi-level RL training
- [ ] **Innovation:** Cognitive hierarchy for agents

#### Weeks 27-29: Prediction-Error-Driven Retrieval (PEDGR)
Following [repograph-research.md Idea #3](../../repograph-research.md):
- [ ] Self-estimated uncertainty monitoring
- [ ] Prediction error detection
- [ ] Error-modulated graph traversal
- [ ] Dynamic retrieval window adaptation
- [ ] **Innovation:** Error-driven context loading

#### Weeks 30-32: Cognitive Workspace (CWCA)
Following [repograph-research.md Idea #4](../../repograph-research.md):
- [ ] Persistent workspace objects
- [ ] Hierarchical buffers (focus/working/long-term)
- [ ] Active memory management
- [ ] Cross-session persistence
- [ ] Multi-level reward signals
- [ ] **Innovation:** External cognition for agents

**Deliverable:** World-class research-backed code intelligence

---

### Phase 4: Multimodal & Enterprise (Weeks 33-40)

#### Weeks 33-36: Multimodal Community-Aware Graph (MCGM)
Following [repograph-research.md Idea #5 + Feature Design #11](../../repograph-research.md):
- [ ] Ingest docs, tests, issues, commits
- [ ] Multimodal community hierarchy
- [ ] Map-reduce reasoning
- [ ] Cross-artifact consistency checks
- [ ] **Innovation:** Holistic code understanding

#### Weeks 37-40: Enterprise Features
- [ ] Incremental indexing (webhooks)
- [ ] Ownership overlays
- [ ] Cross-repository analysis
- [ ] Saved searches & alerts
- [ ] PR check integrations

**Deliverable:** Enterprise-ready platform

---

## 📊 Effort Summary (Research-Aligned)

| Phase | Duration | Features | Team Size |
|-------|----------|----------|-----------|
| **Phase 0: Foundation** | 8 weeks | Fix + research schema | 2 engineers |
| **Phase 1: Graph Core** | 8 weeks | Ego/Impact/Call graphs + streaming | 2 engineers |
| **Phase 2: Semantic** | 6 weeks | Search + QBE + Communities | 2 eng + 1 ML |
| **Phase 3: Research Features** | 14 weeks | ESCM + HCMA + PEDGR + CWCA | 2 eng + 1 ML + 1 research |
| **Phase 4: Multimodal + Enterprise** | 8 weeks | MCGM + integrations | 2-3 engineers |

**Total:** 44 weeks (~11 months) for full research implementation

**Team:**
- 2-3 senior software engineers (full-time)
- 1 ML/AI specialist (from Week 13)
- 1 Research engineer (from Week 19)
- 1 DevOps (part-time throughout)

**Budget:** ~$400-500K for full implementation

---

## 🎯 Success Metrics (Research-Aligned)

### Phase 1 (Graph Core) - Week 12
- ✅ Ego graph p95 < 75ms for depth≤3
- ✅ Impact analysis p95 < 200ms for 10 hops
- ✅ Call graph extraction for 5 languages
- ✅ First visible result ≤ 150ms (streaming)

### Phase 2 (Semantic) - Week 18
- ✅ Hybrid search p95 < 120ms
- ✅ QBE Precision@10 ≥ 0.70
- ✅ Community detection working

### Phase 3 (Research Features) - Week 32
- ✅ ESCM reduces context retrieval time by 40%
- ✅ HCMA handles 10M+ tokens efficiently
- ✅ PEDGR reduces unnecessary retrievals by 30%
- ✅ CWCA improves agent task success by 10pp

### Phase 4 (Multimodal) - Week 40
- ✅ Multimodal answers 20% more complete
- ✅ Cross-artifact inconsistencies detected ≥70%
- ✅ Enterprise features deployed

---

## 🚨 Risk Mitigation

### Technical Risks

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| Research features too complex | High | High | Start simple, iterate; have fallbacks |
| AI/ML expertise shortage | Medium | High | Hire specialist by Week 13; partnerships |
| Performance doesn't meet targets | Medium | Medium | Extensive profiling; query optimization |
| Schema design needs major refactor | Low | High | Design carefully in Phase 0; peer review |

### Schedule Risks

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| Phase 3 takes longer than estimated | High | Medium | Can ship without ESCM/HCMA/PEDGR/CWCA initially |
| Team scaling delays | Medium | Medium | Contract with consultants as backup |
| Scope creep | High | High | Strict feature freeze after roadmap lock |

---

## 💡 Recommendations

### For Next 2 Weeks (Oct 7-20)

**Do This:**
1. ✅ Fix API server (Days 1-3)
2. ✅ Design research-aligned schema (Days 4-10)
3. ✅ Create detailed Phase 1 spec (Days 11-14)

**Don't Do This:**
- ❌ Try to build ESCM/HCMA now (too early)
- ❌ Add features not in research doc
- ❌ Skip schema design (will regret later)

### Strategic Recommendations

**If Time-to-Market is Critical (< 3 months):**
- Follow **Option B** (Simple MVP First)
- Ship basic graph + search in 4 weeks
- Evolve toward research vision over 12 months

**If Building Differentiated Product (6-12 months OK):**
- Follow **Option C** (Hybrid) - **RECOMMENDED**
- Build research-ready foundation
- Ship features incrementally
- Full research vision in 11 months

**If Willing to Bet Big (12+ months):**
- Follow **Option A** (Research-First)
- Build properly from start
- Novel, defensible product
- Attract top talent and funding

---

## 📚 Resources & References

**From repograph-research.md:**
1. Knowledge Graph Based Repository-Level Code Generation [ar5iv.labs.arxiv.org/html/2505.14394]
2. GraphRAG: Complex Data Discovery [microsoft.com/research]
3. RepoHyper: Better Context Retrieval [arxiv.org/html/2403.06095v1]
4. Event Segmentation Theory [pmc.ncbi.nlm.nih.gov/articles/PMC12313307/]
5. Cognitive Workspace for LLMs [arxiv.org/html/2508.13171v1]
6. Hierarchical Memory Transformer [ar5iv.labs.arxiv.org/html/2405.06067v3]
7. EM-LLM: Episodic Memory [em-llm.github.io]

**Implementation Guides:**
- [Feature Designs 1-11](../../repograph-research.md#feature-designs)
- [Technology Designs A-H](../../repograph-research.md#technology-designs)
- [Recommended Sequence](../../repograph-research.md#recommended-sequence-value-first)

---

## ✅ Next Actions

### Immediate (This Week)

1. **Leadership Decision:**
   - Choose Option A, B, or C
   - Allocate budget and team
   - Set timeline expectations

2. **Technical:**
   - Fix API server (blocker)
   - Review and approve research-aligned schema
   - Create detailed Phase 0-1 specifications

3. **Hiring:**
   - If Option A/C: Start recruiting ML specialist
   - If Option C: Plan for research engineer in ~3 months

### This Month

4. **Complete Phase 0:**
   - Working API + tests
   - Research-ready database schema
   - Clear Phase 1 roadmap

5. **Begin Phase 1:**
   - Ego Graph v2 implementation
   - Impact Analysis v2 specification

---

**This roadmap aligns current implementation with the research vision from repograph-research.md**

**Choose your path, commit to it, and execute systematically.**

**Created:** October 6, 2025
**Owner:** Engineering + Product Leadership
**Next Review:** Weekly progress check-ins
