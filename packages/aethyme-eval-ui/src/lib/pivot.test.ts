import { describe, it, expect } from "vitest";
import {
  pivotResults,
  sortConditions,
  rowKeyId,
  CANONICAL_CONDITION_ORDER,
} from "./pivot";
import type { EvalResult } from "./types";

function fixture(overrides: Partial<EvalResult>): EvalResult {
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

describe("pivotResults", () => {
  it("buckets rows by the full (evalType, target, model, reasoning, scenario, batchId) key", () => {
    const rows = [
      fixture({ id: "a", condition: "explore" }),
      fixture({ id: "b", condition: "leverage" }),
    ];
    const m = pivotResults(rows);
    expect(m.rows).toHaveLength(1);
    expect(m.rows[0].cells.size).toBe(2);
    expect(m.rows[0].cells.get("explore")?.runs).toHaveLength(1);
    expect(m.rows[0].cells.get("leverage")?.runs).toHaveLength(1);
  });

  it("treats different reasoning levels as separate rows (research question requires it)", () => {
    const rows = [
      fixture({ id: "a", reasoning: "high" }),
      fixture({ id: "b", reasoning: "low" }),
    ];
    const m = pivotResults(rows);
    expect(m.rows).toHaveLength(2);
  });

  it("treats different scenarios as separate rows", () => {
    const rows = [
      fixture({ id: "a", scenario: "cross-package" }),
      fixture({ id: "b", scenario: null }),
    ];
    const m = pivotResults(rows);
    expect(m.rows).toHaveLength(2);
  });

  it("aggregates repetitions sharing (key, condition) into one cell with mean+stdev", () => {
    const rows = [
      fixture({ id: "a", batchId: "b1", runIndex: 1, runsInBatch: 3, qualityScore: 70, totalTokens: 1000 }),
      fixture({ id: "b", batchId: "b1", runIndex: 2, runsInBatch: 3, qualityScore: 80, totalTokens: 1200 }),
      fixture({ id: "c", batchId: "b1", runIndex: 3, runsInBatch: 3, qualityScore: 90, totalTokens: 800 }),
    ];
    const m = pivotResults(rows);
    expect(m.rows).toHaveLength(1);
    const cell = m.rows[0].cells.get("explore")!;
    expect(cell.runs).toHaveLength(3);
    expect(cell.qualityScore).toBeCloseTo(80);
    expect(cell.totalTokens).toBeCloseTo(1000);
    // sample stdev of [70,80,90] = 10
    expect(cell.qualityStdev).toBeCloseTo(10);
  });

  it("rolls up deliverable status worst-wins across cell runs", () => {
    const rows = [
      fixture({ id: "a", deliverableStatus: "success" }),
      fixture({ id: "b", deliverableStatus: "degraded" }),
    ];
    const m = pivotResults(rows);
    expect(m.rows[0].cells.get("explore")?.deliverableStatus).toBe("degraded");
  });

  it("preserves condition encounter order in matrix.conditions", () => {
    const rows = [
      fixture({ id: "a", condition: "leverage" }),
      fixture({ id: "b", condition: "control-cto-off" }),
      fixture({ id: "c", condition: "explore" }),
    ];
    const m = pivotResults(rows);
    expect(m.conditions).toEqual(["leverage", "control-cto-off", "explore"]);
  });

  it("emits one cell per condition per row, missing cells are absent from the map", () => {
    // Controls present, explore missing for one row but present for another.
    const rows = [
      fixture({ id: "a", target: "grc", condition: "control-cto-off" }),
      fixture({ id: "b", target: "grc", condition: "control-cto-on" }),
      fixture({ id: "c", target: "mediawiki", condition: "control-cto-off" }),
      fixture({ id: "d", target: "mediawiki", condition: "explore" }),
    ];
    const m = pivotResults(rows);
    const grc = m.rows.find((r) => r.key.target === "grc")!;
    const mw = m.rows.find((r) => r.key.target === "mediawiki")!;
    expect(grc.cells.has("explore")).toBe(false);
    expect(mw.cells.has("control-cto-on")).toBe(false);
    expect(mw.cells.has("explore")).toBe(true);
  });

  it("handles null qualityScore gracefully — cell mean is null, not 0", () => {
    const rows = [
      fixture({ id: "a", qualityScore: null }),
      fixture({ id: "b", qualityScore: null }),
    ];
    const m = pivotResults(rows);
    expect(m.rows[0].cells.get("explore")?.qualityScore).toBeNull();
  });
});

describe("rowKeyId", () => {
  it("is stable across calls for equivalent keys", () => {
    const a = rowKeyId({
      evalType: "bug-fix", target: "grc", model: "haiku",
      reasoning: "high", scenario: null, batchId: null,
    });
    const b = rowKeyId({
      evalType: "bug-fix", target: "grc", model: "haiku",
      reasoning: "high", scenario: null, batchId: null,
    });
    expect(a).toBe(b);
  });

  it("differs when any field differs", () => {
    const base = {
      evalType: "bug-fix", target: "grc", model: "haiku",
      reasoning: "high", scenario: null, batchId: null,
    };
    const ids = new Set([
      rowKeyId(base),
      rowKeyId({ ...base, evalType: "explain-repo" }),
      rowKeyId({ ...base, target: "mediawiki" }),
      rowKeyId({ ...base, scenario: "cross-package" }),
      rowKeyId({ ...base, batchId: "b1" }),
    ]);
    expect(ids.size).toBe(5);
  });
});

describe("sortConditions", () => {
  it("orders known conditions by CANONICAL_CONDITION_ORDER", () => {
    const found = ["leverage", "control-cto-on", "explore", "control-cto-off"];
    expect(sortConditions(found)).toEqual([
      "control-cto-off", "control-cto-on", "explore", "leverage",
    ]);
  });

  it("appends unknown conditions alphabetically after known", () => {
    const found = ["explore", "zzz-custom", "aaa-experimental"];
    const sorted = sortConditions(found);
    expect(sorted.slice(0, 1)).toEqual(["explore"]);
    expect(sorted.slice(1)).toEqual(["aaa-experimental", "zzz-custom"]);
  });

  it("keeps full canonical order coverage when all are present", () => {
    const sorted = sortConditions([...CANONICAL_CONDITION_ORDER]);
    expect(sorted).toEqual([...CANONICAL_CONDITION_ORDER]);
  });
});
