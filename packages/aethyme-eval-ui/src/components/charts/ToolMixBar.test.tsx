import { describe, it, expect } from "vitest";
import { render } from "@testing-library/react";
import ToolMixBar, { toolColor } from "./ToolMixBar";

describe("ToolMixBar", () => {
  it("renders nothing when tools is empty", () => {
    const { container } = render(<ToolMixBar tools={[]} />);
    expect(container.querySelector("svg")).toBeNull();
  });

  it("renders one rect per tool, widths sum to the full width", () => {
    const { container } = render(
      <ToolMixBar tools={[{ name: "Read", count: 3 }, { name: "Bash", count: 1 }]} width={100} height={8} />,
    );
    const rects = container.querySelectorAll("rect");
    expect(rects).toHaveLength(2);
    const totalW = Array.from(rects).reduce((s, r) => s + parseFloat(r.getAttribute("width") || "0"), 0);
    expect(totalW).toBeCloseTo(100);
  });

  it("orders segments by count descending (biggest tool on the left)", () => {
    const { container } = render(
      <ToolMixBar tools={[{ name: "a", count: 1 }, { name: "big", count: 10 }]} width={100} />,
    );
    const rects = Array.from(container.querySelectorAll("rect"));
    // First rect (x=0) should belong to the biggest tool.
    const firstX = parseFloat(rects[0].getAttribute("x") || "-1");
    expect(firstX).toBe(0);
    const firstWidth = parseFloat(rects[0].getAttribute("width") || "0");
    expect(firstWidth).toBeGreaterThan(50);
  });

  it("exposes a tool-mix summary in aria-label", () => {
    const { container } = render(
      <ToolMixBar tools={[{ name: "Read", count: 3 }, { name: "Bash", count: 1 }]} width={100} />,
    );
    const label = container.querySelector("svg")?.getAttribute("aria-label");
    expect(label).toContain("Read 75%");
    expect(label).toContain("Bash 25%");
  });

  it("toolColor is stable for a given name", () => {
    expect(toolColor("Read")).toBe(toolColor("Read"));
    // And ideally different names map to different colors most of the time.
    // (Not guaranteed for every pair under a small palette — just spot-check.)
    expect(toolColor("Read")).not.toBe(toolColor("Bash"));
  });
});
