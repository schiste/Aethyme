import { describe, it, expect } from "vitest";
import { annotate, verdictStyle, DEFAULT_MMD } from "./significance";

describe("annotate", () => {
  it("flags delta >= MMD as meaningful", () => {
    const r = annotate({
      treatment: 85, control: 70,
      treatmentN: 3, controlN: 3,
      minimumMeaningfulDelta: 5,
      higherIsBetter: true,
    });
    expect(r.verdict).toBe("meaningful");
    expect(r.delta).toBe(15);
    expect(r.mmdApplied).toBe(5);
    expect(r.mmdPreRegistered).toBe(true);
  });

  it("flags delta < MMD as within-noise", () => {
    const r = annotate({
      treatment: 72, control: 70,
      treatmentN: 3, controlN: 3,
      minimumMeaningfulDelta: 5,
      higherIsBetter: true,
    });
    expect(r.verdict).toBe("within-noise");
    expect(r.delta).toBe(2);
  });

  it("returns insufficient when either arm has n<2", () => {
    const r = annotate({
      treatment: 85, control: 70,
      treatmentN: 1, controlN: 3,
      minimumMeaningfulDelta: 5,
      higherIsBetter: true,
    });
    expect(r.verdict).toBe("insufficient");
    expect(r.delta).toBe(15);
  });

  it("returns null verdict when either side is missing a score", () => {
    const r = annotate({
      treatment: null, control: 70,
      treatmentN: 3, controlN: 3,
      minimumMeaningfulDelta: 5,
      higherIsBetter: true,
    });
    expect(r.verdict).toBeNull();
    expect(r.delta).toBeNull();
  });

  it("falls back to DEFAULT_MMD when MMD is null/zero", () => {
    const r = annotate({
      treatment: 76, control: 70,
      treatmentN: 3, controlN: 3,
      minimumMeaningfulDelta: null,
      higherIsBetter: true,
    });
    expect(r.mmdApplied).toBe(DEFAULT_MMD);
    expect(r.mmdPreRegistered).toBe(false);
    // 6 >= 5 (default) → meaningful
    expect(r.verdict).toBe("meaningful");
  });

  it("inverts sign semantics when higherIsBetter=false (tokens/cost)", () => {
    // Treatment uses FEWER tokens — should register as positive (win).
    const r = annotate({
      treatment: 800, control: 1000,
      treatmentN: 3, controlN: 3,
      minimumMeaningfulDelta: 100,
      higherIsBetter: false,
    });
    expect(r.verdict).toBe("meaningful");
    expect(r.delta).toBe(200); // flipped: treatment saves 200 tokens
  });

  it("treats raw delta of exactly MMD as meaningful (inclusive boundary)", () => {
    const r = annotate({
      treatment: 75, control: 70,
      treatmentN: 3, controlN: 3,
      minimumMeaningfulDelta: 5,
      higherIsBetter: true,
    });
    expect(r.verdict).toBe("meaningful");
  });

  it("preserves sign when treatment loses", () => {
    const r = annotate({
      treatment: 60, control: 80,
      treatmentN: 3, controlN: 3,
      minimumMeaningfulDelta: 5,
      higherIsBetter: true,
    });
    expect(r.verdict).toBe("meaningful");
    expect(r.delta).toBe(-20);
  });
});

describe("verdictStyle", () => {
  it("meaningful + positive delta → green ▲", () => {
    const s = verdictStyle("meaningful", 10);
    expect(s.label).toBe("▲");
    expect(s.color).toContain("green");
  });

  it("meaningful + negative delta → red ▼", () => {
    const s = verdictStyle("meaningful", -10);
    expect(s.label).toBe("▼");
    expect(s.color).toContain("red");
  });

  it("within-noise → muted ≈", () => {
    const s = verdictStyle("within-noise", 2);
    expect(s.label).toBe("≈");
    expect(s.color).toContain("muted");
  });

  it("insufficient → muted n<2", () => {
    const s = verdictStyle("insufficient", 10);
    expect(s.label).toBe("n<2");
  });

  it("null verdict → muted em-dash", () => {
    const s = verdictStyle(null, null);
    expect(s.label).toBe("—");
  });
});
