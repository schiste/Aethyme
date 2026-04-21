import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import Charts from "./Charts";
import type { EvalResult } from "../lib/types";

vi.mock("../lib/api", () => ({ fetchResults: vi.fn() }));
import * as api from "../lib/api";

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
    cost: 0.10,
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

describe("Charts page", () => {
  beforeEach(() => {
    vi.mocked(api.fetchResults).mockReset();
  });

  it("renders scatter with one point per batch-condition", async () => {
    vi.mocked(api.fetchResults).mockResolvedValue([
      row({ id: "a", condition: "control-cto-off", batchId: "b1", runsInBatch: 3, qualityScore: 70, cost: 0.10 }),
      row({ id: "b", condition: "control-cto-off", batchId: "b1", runsInBatch: 3, qualityScore: 75, cost: 0.11, runIndex: 2 }),
      row({ id: "c", condition: "explore",         batchId: "b1", runsInBatch: 3, qualityScore: 85, cost: 0.13 }),
      row({ id: "d", condition: "explore",         batchId: "b1", runsInBatch: 3, qualityScore: 82, cost: 0.12, runIndex: 2 }),
    ]);
    render(<MemoryRouter><Charts /></MemoryRouter>);
    await waitFor(() => {
      // Panel title shows the current axes
      expect(screen.getByText(/Quality score vs Cost \(USD\)/)).toBeInTheDocument();
    });
    expect(screen.getByText(/2 points/)).toBeInTheDocument();
    const svg = screen.getByRole("img", { name: /Scatter plot/ });
    // 2 points → 2 circles in the scatter body
    const circles = svg.querySelectorAll("circle");
    expect(circles.length).toBe(2);
  });

  it("shows empty state when no rows have both x and y", async () => {
    vi.mocked(api.fetchResults).mockResolvedValue([
      row({ qualityScore: null, cost: 0.5 }), // y missing
    ]);
    render(<MemoryRouter><Charts /></MemoryRouter>);
    await waitFor(() => {
      expect(screen.getByText(/No points have both/)).toBeInTheDocument();
    });
  });

  it("renders legend entries for each condition in the plotted points", async () => {
    vi.mocked(api.fetchResults).mockResolvedValue([
      row({ id: "a", condition: "control-cto-off", qualityScore: 60, cost: 0.1 }),
      row({ id: "b", condition: "explore",         qualityScore: 80, cost: 0.1 }),
      row({ id: "c", condition: "leverage",        qualityScore: 85, cost: 0.15 }),
    ]);
    render(<MemoryRouter><Charts /></MemoryRouter>);
    await waitFor(() => {
      expect(screen.getByText(/3 points/)).toBeInTheDocument();
    });
    // Legend text appears in scatter; both chart and legend use condition names
    expect(screen.getAllByText("control-cto-off").length).toBeGreaterThan(0);
    expect(screen.getAllByText("explore").length).toBeGreaterThan(0);
    expect(screen.getAllByText("leverage").length).toBeGreaterThan(0);
  });
});
