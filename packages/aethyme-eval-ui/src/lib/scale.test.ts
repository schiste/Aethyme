import { describe, it, expect } from "vitest";
import {
  linearScale,
  bandScale,
  niceTicks,
  extent,
  padExtent,
} from "./scale";

describe("linearScale", () => {
  it("maps domain endpoints to range endpoints", () => {
    const s = linearScale([0, 100], [0, 500]);
    expect(s(0)).toBe(0);
    expect(s(100)).toBe(500);
    expect(s(50)).toBe(250);
  });

  it("inverts correctly (round-trip)", () => {
    const s = linearScale([10, 20], [0, 100]);
    expect(s.invert(s(14))).toBeCloseTo(14);
    expect(s.invert(s(19))).toBeCloseTo(19);
  });

  it("handles inverted ranges (y-axis top-to-bottom)", () => {
    const s = linearScale([0, 100], [400, 0]);
    expect(s(0)).toBe(400);
    expect(s(100)).toBe(0);
    expect(s(50)).toBe(200);
  });

  it("survives zero-span domain without NaN", () => {
    const s = linearScale([5, 5], [0, 100]);
    expect(Number.isFinite(s(5))).toBe(true);
    expect(Number.isFinite(s(10))).toBe(true);
  });
});

describe("niceTicks", () => {
  it("returns round step values spanning the range", () => {
    // The "nicest" step near 100/(5-1)=25 snaps to 20, so we get
    // 6 ticks at step 20 — picked over 4 ticks at step 25 because 20 is
    // a rounder multiple of 10. Either is reasonable; we pin the behavior.
    const ticks = niceTicks(0, 100, 5);
    expect(ticks).toEqual([0, 20, 40, 60, 80, 100]);
  });

  it("works on non-round domains", () => {
    const ticks = niceTicks(0.3, 0.87, 5);
    // All ticks should be in [0.3, 0.87] and evenly spaced.
    expect(ticks.length).toBeGreaterThan(2);
    for (const t of ticks) {
      expect(t).toBeGreaterThanOrEqual(0.3);
      expect(t).toBeLessThanOrEqual(0.87);
    }
    // Even spacing
    const steps = ticks.slice(1).map((t, i) => t - ticks[i]);
    const first = steps[0];
    for (const s of steps) expect(s).toBeCloseTo(first, 6);
  });

  it("returns [min] when min==max", () => {
    expect(niceTicks(7, 7)).toEqual([7]);
  });

  it("handles small negative-to-positive ranges", () => {
    const ticks = niceTicks(-1, 1, 5);
    expect(ticks).toContain(0);
  });
});

describe("bandScale", () => {
  it("centers each category in its band", () => {
    const s = bandScale(["a", "b", "c"], [0, 300], 0);
    // 3 bands of 100 each, centers at 50, 150, 250
    expect(s("a")).toBe(50);
    expect(s("b")).toBe(150);
    expect(s("c")).toBe(250);
  });

  it("applies padding between bands", () => {
    const s = bandScale(["a", "b"], [0, 200], 0.2);
    // Centers unchanged by padding; bandwidth shrinks.
    expect(s("a")).toBe(50);
    expect(s("b")).toBe(150);
    expect(s.bandwidth).toBeCloseTo(80);
  });

  it("returns NaN for unknown category (caller must guard)", () => {
    const s = bandScale(["a"], [0, 100]);
    expect(Number.isNaN(s("missing"))).toBe(true);
  });
});

describe("extent", () => {
  it("ignores nulls, NaN, Infinity", () => {
    expect(extent([1, null, 3, NaN, 2, Infinity, undefined])).toEqual([1, 3]);
  });

  it("returns null when no finite values", () => {
    expect(extent([null, NaN, undefined])).toBeNull();
  });

  it("handles single value", () => {
    expect(extent([42])).toEqual([42, 42]);
  });
});

describe("padExtent", () => {
  it("expands by a fraction of the span", () => {
    expect(padExtent([0, 100], 0.1)).toEqual([-10, 110]);
  });

  it("uses a non-zero pad when span is zero", () => {
    const [a, b] = padExtent([50, 50], 0.1);
    expect(a).toBeLessThan(50);
    expect(b).toBeGreaterThan(50);
  });
});
