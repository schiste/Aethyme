import { describe, it, expect } from "vitest";
import { buildParetoPoints, parseDuration, conditionColor } from "./chartData";
import type { EvalResult } from "./types";

function row(overrides: Partial<EvalResult>): EvalResult {
  return {
    id: "r",
    date: "2026-04-20",
    evalType: "bug-fix",
    target: "grc",
    model: "haiku",
    condition: "explore",
    reasoning: "high",
    cto: "on",
    score: 0,
    turns: 0,
    toolCalls: 0,
    totalTokens: 1000,
    inputTokens: 500,
    outputTokens: 500,
    cacheRead: 0,
    cacheCreate: 0,
    cost: 0.1,
    duration: "30s",
    fixed: true,
    scenario: null,
    output: null,
    toolBreakdown: null,
    prompt: null,
    runId: null,
    qualityScore: 80,
    ...overrides,
  };
}

describe("parseDuration", () => {
  it("parses bare seconds", () => {
    expect(parseDuration("30s")).toBe(30);
  });
  it("parses minutes + seconds", () => {
    expect(parseDuration("1m 45s")).toBe(105);
  });
  it("parses minutes only", () => {
    expect(parseDuration("3m")).toBe(180);
  });
  it("returns null for '-'", () => {
    expect(parseDuration("-")).toBeNull();
  });
  it("returns null for null/undefined/empty", () => {
    expect(parseDuration(null)).toBeNull();
    expect(parseDuration(undefined)).toBeNull();
    expect(parseDuration("")).toBeNull();
  });
});

describe("buildParetoPoints", () => {
  it("emits one point per batch-condition tuple, aggregating repetitions", () => {
    const rows = [
      row({ id: "a", batchId: "b1", runsInBatch: 3, runIndex: 1, qualityScore: 70, cost: 0.10 }),
      row({ id: "b", batchId: "b1", runsInBatch: 3, runIndex: 2, qualityScore: 80, cost: 0.12 }),
      row({ id: "c", batchId: "b1", runsInBatch: 3, runIndex: 3, qualityScore: 90, cost: 0.14 }),
    ];
    const points = buildParetoPoints(rows);
    expect(points).toHaveLength(1);
    const p = points[0];
    expect(p.n).toBe(3);
    expect(p.qualityScore).toBeCloseTo(80);
    expect(p.cost).toBeCloseTo(0.12);
    expect(p.qualityStdev).toBeCloseTo(10);
  });

  it("treats rows without batchId as independent singletons (no merging)", () => {
    const rows = [
      row({ id: "a", condition: "explore", qualityScore: 50 }),
      row({ id: "b", condition: "explore", qualityScore: 90 }),
    ];
    const points = buildParetoPoints(rows);
    expect(points).toHaveLength(2);
  });

  it("splits by condition even within one batch", () => {
    const rows = [
      row({ id: "a", batchId: "b1", condition: "control-cto-off", qualityScore: 60 }),
      row({ id: "b", batchId: "b1", condition: "explore",         qualityScore: 85 }),
    ];
    const points = buildParetoPoints(rows);
    expect(points).toHaveLength(2);
    expect(points.map((p) => p.condition).sort()).toEqual(["control-cto-off", "explore"]);
  });

  it("parses duration into seconds per point", () => {
    const rows = [
      row({ batchId: "b1", duration: "30s" }),
      row({ batchId: "b1", duration: "1m", runIndex: 2 }),
    ];
    const [p] = buildParetoPoints(rows);
    expect(p.durationSeconds).toBeCloseTo(45);
  });

  it("rolls up deliverable status worst-wins", () => {
    const rows = [
      row({ id: "a", batchId: "b1", deliverableStatus: "success" }),
      row({ id: "b", batchId: "b1", deliverableStatus: "degraded" }),
    ];
    expect(buildParetoPoints(rows)[0].deliverableStatus).toBe("degraded");
  });
});

describe("conditionColor", () => {
  it("returns distinct colors for each canonical condition", () => {
    const conditions = ["control-cto-off", "control-cto-on", "explore", "leverage", "task-conditioned"];
    const colors = new Set(conditions.map(conditionColor));
    expect(colors.size).toBe(conditions.length);
  });
  it("falls back on unknown conditions without throwing", () => {
    expect(conditionColor("some-future-condition")).toBe("#f97316");
  });
});
