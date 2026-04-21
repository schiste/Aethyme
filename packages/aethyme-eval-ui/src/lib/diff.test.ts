import { describe, it, expect } from "vitest";
import { diffLines } from "./diff";

describe("diffLines", () => {
  it("returns 100% similarity for identical inputs", () => {
    const a = "line one\nline two\nline three";
    const { stats } = diffLines(a, a);
    expect(stats.similarityPct).toBe(100);
    expect(stats.onlyA).toBe(0);
    expect(stats.onlyB).toBe(0);
    expect(stats.matched).toBe(3);
  });

  it("handles fully disjoint inputs as 0% similarity", () => {
    const a = "apple\nbanana\ncherry";
    const b = "dog\nelephant\nfrog";
    const { stats } = diffLines(a, b);
    expect(stats.similarityPct).toBe(0);
    expect(stats.matched).toBe(0);
    expect(stats.onlyA).toBe(3);
    expect(stats.onlyB).toBe(3);
  });

  it("preserves alignment on a common prefix with divergent tails", () => {
    const a = "header\nalpha\nbeta";
    const b = "header\ngamma\ndelta";
    const { rows } = diffLines(a, b);
    expect(rows[0].op).toBe("same");
    expect(rows[0].aText).toBe("header");
    // The tail produces a pair of onlyA/onlyB in some order
    const tailOps = rows.slice(1).map((r) => r.op).sort();
    expect(tailOps).toEqual(["onlyA", "onlyA", "onlyB", "onlyB"]);
  });

  it("handles a pure insertion in B", () => {
    const a = "one\ntwo\nthree";
    const b = "one\ntwo\nINSERTED\nthree";
    const { rows, stats } = diffLines(a, b);
    expect(stats.matched).toBe(3);
    expect(stats.onlyA).toBe(0);
    expect(stats.onlyB).toBe(1);
    // Similarity is matched/max(n,m) = 3/4 = 75%
    expect(stats.similarityPct).toBe(75);
    const inserted = rows.find((r) => r.op === "onlyB")!;
    expect(inserted.bText).toBe("INSERTED");
  });

  it("handles empty A and non-empty B as all-onlyB", () => {
    const { rows, stats } = diffLines("", "hello\nworld");
    expect(stats.matched).toBe(0);
    expect(rows.every((r) => r.op === "onlyB")).toBe(true);
    expect(rows).toHaveLength(2);
  });

  it("handles both empty as an empty diff", () => {
    const { rows, stats } = diffLines("", "");
    expect(rows).toHaveLength(0);
    expect(stats.matched).toBe(0);
  });

  it("gives line indices that let the viewer render line numbers", () => {
    const { rows } = diffLines("x\ny", "x\nZ\ny");
    // row 0: "x" === "x" → aIndex=0, bIndex=0
    // row 1: "Z" only in B → aIndex=null, bIndex=1
    // row 2: "y" === "y" → aIndex=1, bIndex=2
    expect(rows[0]).toMatchObject({ op: "same", aIndex: 0, bIndex: 0 });
    expect(rows[1]).toMatchObject({ op: "onlyB", aIndex: null, bIndex: 1 });
    expect(rows[2]).toMatchObject({ op: "same", aIndex: 1, bIndex: 2 });
  });
});
