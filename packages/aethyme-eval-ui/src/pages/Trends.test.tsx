import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import Trends from "./Trends";
import type { EvalResult } from "../lib/types";

vi.mock("../lib/api", () => ({ fetchResults: vi.fn() }));
import * as api from "../lib/api";

function row(overrides: Partial<EvalResult>): EvalResult {
  return {
    id: "r",
    date: "2026-04-20T12:00:00",
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

describe("Trends page", () => {
  beforeEach(() => {
    vi.mocked(api.fetchResults).mockReset();
  });

  it("renders one card per (eval_type, target, model) with enough historical points", async () => {
    vi.mocked(api.fetchResults).mockResolvedValue([
      row({ id: "a", date: "2026-03-01", qualityScore: 60 }),
      row({ id: "b", date: "2026-04-01", qualityScore: 70 }),
      row({ id: "c", date: "2026-04-20", qualityScore: 82 }),
    ]);
    render(<MemoryRouter><Trends /></MemoryRouter>);
    await waitFor(() => {
      expect(screen.getByText(/bug-fix · grc · haiku/)).toBeInTheDocument();
    });
    // One sparkline per condition. 1 condition, 1 sparkline SVG.
    const sparks = screen.getAllByRole("img", { name: /Trend of \d+ points/ });
    expect(sparks).toHaveLength(1);
  });

  it("hides configs with only 1 historical point (nothing to trend)", async () => {
    vi.mocked(api.fetchResults).mockResolvedValue([
      row({ id: "a", qualityScore: 60 }),
    ]);
    render(<MemoryRouter><Trends /></MemoryRouter>);
    await waitFor(() => {
      expect(screen.getByText(/No configs with ≥2 historical points/)).toBeInTheDocument();
    });
  });

  it("separates sparklines per condition within a (type,target,model) group", async () => {
    vi.mocked(api.fetchResults).mockResolvedValue([
      row({ id: "a", date: "2026-03-01", condition: "control-cto-off", qualityScore: 55 }),
      row({ id: "b", date: "2026-04-01", condition: "control-cto-off", qualityScore: 60 }),
      row({ id: "c", date: "2026-03-01", condition: "explore",         qualityScore: 70 }),
      row({ id: "d", date: "2026-04-01", condition: "explore",         qualityScore: 85 }),
    ]);
    render(<MemoryRouter><Trends /></MemoryRouter>);
    await waitFor(() => {
      expect(screen.getByText(/bug-fix · grc · haiku/)).toBeInTheDocument();
    });
    // Both condition labels shown in the card
    expect(screen.getByText("control-cto-off")).toBeInTheDocument();
    expect(screen.getByText("explore")).toBeInTheDocument();
    // 2 sparkline SVGs
    const sparks = screen.getAllByRole("img", { name: /Trend of \d+ points/ });
    expect(sparks).toHaveLength(2);
  });
});
