import { describe, it, expect } from "vitest";
import {
  evaluateReadiness,
  projectCost,
  DEFAULT_CONDITIONS_PER_RUN,
} from "./readiness";
import type { Repository } from "./types";

function repo(overrides: Partial<Repository> = {}): Repository {
  return {
    name: "grc",
    target: "grc",
    controlPath: "/tmp/control",
    aethymePath: "/tmp/aethyme",
    setupSource: null,
    setupCommit: null,
    setupDest: "/tmp",
    validationStatus: "valid",
    controlClean: { clean: true, issues: [] },
    aethymeIndex: {
      indexed: true,
      date: new Date().toISOString(),
      sizeMb: 10,
      path: "/tmp/.aethyme/graph.db",
    },
    snippets: {
      present: true,
      date: new Date().toISOString(),
      totalSnippets: 20,
      aethymeSnippets: 5,
    },
    ...overrides,
  };
}

describe("evaluateReadiness", () => {
  it("reports ok + canLaunch when all checks pass", () => {
    const r = evaluateReadiness(repo());
    expect(r.overall).toBe("ok");
    expect(r.canLaunch).toBe(true);
  });

  it("flags contaminated control as a blocker", () => {
    const r = evaluateReadiness(repo({
      controlClean: { clean: false, issues: ["Skill found: .codex/skills/aethyme"] },
    }));
    expect(r.overall).toBe("blocker");
    expect(r.canLaunch).toBe(false);
    const check = r.checks.find((c) => c.key === "control-clean");
    expect(check?.status).toBe("blocker");
  });

  it("flags missing graph index as a blocker", () => {
    const r = evaluateReadiness(repo({
      aethymeIndex: { indexed: false, date: null, path: "/tmp/.aethyme/graph.db" },
    }));
    expect(r.overall).toBe("blocker");
    expect(r.canLaunch).toBe(false);
  });

  it("flags stale graph index (>7d) as a warning, not a blocker", () => {
    const old = new Date(Date.now() - 10 * 24 * 60 * 60 * 1000).toISOString();
    const r = evaluateReadiness(repo({
      aethymeIndex: { indexed: true, date: old, sizeMb: 10, path: "/tmp/.aethyme/graph.db" },
    }));
    expect(r.canLaunch).toBe(true);
    expect(r.overall).toBe("warning");
  });

  it("flags missing snippets as a warning, not a blocker", () => {
    const r = evaluateReadiness(repo({
      snippets: { present: false, date: null, totalSnippets: 0, aethymeSnippets: 0 },
    }));
    expect(r.canLaunch).toBe(true);
    expect(r.overall).toBe("warning");
  });

  it("returns an unknown state when no repo is selected", () => {
    const r = evaluateReadiness(null);
    expect(r.overall).toBe("unknown");
    expect(r.canLaunch).toBe(false);
  });
});

describe("projectCost", () => {
  it("uses anchored model×eval-type estimate when available", () => {
    const p = projectCost({
      model: "haiku", evalType: "bug-fix",
      runs: 1, useJudge: false, judgeSamples: 3,
    });
    expect(p.confidence).toBe("anchored");
    // 0.35 × 5 conditions × 1 run = 1.75
    expect(p.agentCostUsd).toBeCloseTo(1.75);
    expect(p.judgeCostUsd).toBe(0);
    expect(p.totalUsd).toBeCloseTo(1.75);
  });

  it("scales with runs (batched repetitions)", () => {
    const p = projectCost({
      model: "haiku", evalType: "bug-fix",
      runs: 3, useJudge: false, judgeSamples: 0,
    });
    expect(p.agentCostUsd).toBeCloseTo(1.75 * 3);
  });

  it("adds judge cost per (sample × condition × run)", () => {
    const p = projectCost({
      model: "haiku", evalType: "bug-fix",
      runs: 2, useJudge: true, judgeSamples: 3,
    });
    // judge = 0.02 × 3 samples × 5 conditions × 2 runs = 0.60
    expect(p.judgeCostUsd).toBeCloseTo(0.6);
  });

  it("falls back to the model default for unseen eval types", () => {
    const p = projectCost({
      model: "haiku", evalType: "some-new-eval",
      runs: 1, useJudge: false, judgeSamples: 0,
    });
    expect(p.confidence).toBe("fallback");
  });

  it("marks unknown models as guess", () => {
    const p = projectCost({
      model: "mystery-model", evalType: "bug-fix",
      runs: 1, useJudge: false, judgeSamples: 0,
    });
    expect(p.confidence).toBe("guess");
    expect(p.totalUsd).toBeGreaterThan(0);
  });

  it("respects conditionsPerRun override", () => {
    const p = projectCost({
      model: "haiku", evalType: "bug-fix",
      runs: 1, useJudge: false, judgeSamples: 0,
      conditionsPerRun: 1,
    });
    expect(p.agentCostUsd).toBeCloseTo(0.35);
  });

  it("handles runs=0 as runs=1 (never negative-multiplies)", () => {
    const p = projectCost({
      model: "haiku", evalType: "bug-fix",
      runs: 0, useJudge: false, judgeSamples: 0,
    });
    expect(p.agentCostUsd).toBeGreaterThan(0);
  });
});

describe("DEFAULT_CONDITIONS_PER_RUN", () => {
  it("matches the eval protocol (5 conditions)", () => {
    expect(DEFAULT_CONDITIONS_PER_RUN).toBe(5);
  });
});
