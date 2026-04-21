import { describe, it, expect } from "vitest";
import { render } from "@testing-library/react";
import JudgeDotStrip from "./JudgeDotStrip";

describe("JudgeDotStrip", () => {
  it("renders nothing when samples is empty", () => {
    const { container } = render(<JudgeDotStrip samples={[]} mean={0} />);
    expect(container.querySelector("svg")).toBeNull();
  });

  it("renders one circle per sample plus a mean marker line", () => {
    const { container } = render(<JudgeDotStrip samples={[40, 45, 50]} mean={45} />);
    const svg = container.querySelector("svg")!;
    expect(svg).not.toBeNull();
    expect(svg.querySelectorAll("circle")).toHaveLength(3);
    // 2 lines: baseline + mean marker
    expect(svg.querySelectorAll("line")).toHaveLength(2);
  });

  it("handles all-identical samples without collapsing the domain", () => {
    // The chart would divide-by-zero if we didn't pad the domain.
    const { container } = render(<JudgeDotStrip samples={[50, 50, 50]} mean={50} />);
    const svg = container.querySelector("svg")!;
    const circles = svg.querySelectorAll("circle");
    expect(circles).toHaveLength(3);
    // All three circles should sit at the same x (the padded center).
    const xs = Array.from(circles).map((c) => c.getAttribute("cx"));
    expect(new Set(xs).size).toBe(1);
  });

  it("exposes samples + mean in the aria-label for screen readers", () => {
    const { container } = render(<JudgeDotStrip samples={[40, 50, 60]} mean={50} />);
    const svg = container.querySelector("svg")!;
    const label = svg.getAttribute("aria-label");
    expect(label).toContain("40.0");
    expect(label).toContain("50.0");
    expect(label).toContain("60.0");
    expect(label).toContain("mean 50.0");
  });
});
