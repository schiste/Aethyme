import { describe, it, expect } from "vitest";
import { quantile, boxSummary } from "./stats";

describe("quantile (linear, R type 7)", () => {
  it("returns endpoints at p=0 and p=1", () => {
    const vals = [1, 2, 3, 4, 5];
    expect(quantile(vals, 0)).toBe(1);
    expect(quantile(vals, 1)).toBe(5);
  });
  it("returns median at p=0.5 for odd-length", () => {
    expect(quantile([1, 2, 3], 0.5)).toBe(2);
  });
  it("interpolates between two middle values for even-length", () => {
    expect(quantile([1, 2, 3, 4], 0.5)).toBe(2.5);
  });
  it("matches R's known quartiles on a classic example", () => {
    // quantile(c(6, 7, 15, 36, 39, 40, 41, 42, 43, 47, 49), c(.25, .5, .75))
    // R gives 25.5, 40, 42.5
    const vals = [6, 7, 15, 36, 39, 40, 41, 42, 43, 47, 49];
    expect(quantile(vals, 0.25)).toBeCloseTo(25.5);
    expect(quantile(vals, 0.5)).toBe(40);
    expect(quantile(vals, 0.75)).toBeCloseTo(42.5);
  });
  it("returns the single value on n=1 input", () => {
    expect(quantile([7], 0.5)).toBe(7);
  });
  it("throws on empty input", () => {
    expect(() => quantile([], 0.5)).toThrow();
  });
});

describe("boxSummary", () => {
  it("computes a summary with no outliers on a clean distribution", () => {
    const s = boxSummary([10, 12, 14, 16, 18, 20, 22, 24, 26])!;
    expect(s).not.toBeNull();
    expect(s.five.n).toBe(9);
    expect(s.five.median).toBe(18);
    expect(s.outliers).toEqual([]);
    expect(s.whiskerLo).toBe(10);
    expect(s.whiskerHi).toBe(26);
  });

  it("flags values beyond 1.5×IQR as outliers", () => {
    // IQR clearly demarcated. 100 is far above the cluster.
    const s = boxSummary([10, 12, 14, 16, 18, 20, 100])!;
    expect(s.outliers).toContain(100);
    expect(s.whiskerHi).toBeLessThan(100);
  });

  it("returns null on fewer than 2 values", () => {
    expect(boxSummary([5])).toBeNull();
    expect(boxSummary([])).toBeNull();
    expect(boxSummary([null, null, null])).toBeNull();
  });

  it("ignores null/NaN/undefined entries before computing", () => {
    const s = boxSummary([1, null, NaN, 2, undefined, 3])!;
    expect(s).not.toBeNull();
    expect(s.five.n).toBe(3);
    expect(s.five.median).toBe(2);
  });

  it("handles n=2 — whiskers equal the two values, no outliers possible", () => {
    const s = boxSummary([5, 10])!;
    expect(s.five.n).toBe(2);
    expect(s.five.min).toBe(5);
    expect(s.five.max).toBe(10);
    expect(s.outliers).toEqual([]);
  });
});
