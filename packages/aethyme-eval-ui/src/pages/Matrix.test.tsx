import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import Matrix from "./Matrix";
import type { EvalResult } from "../lib/types";

function row(overrides: Partial<EvalResult>): EvalResult {
  return {
    id: "r",
    date: "2026-04-20",
    evalType: "bug-fix",
    target: "grc",
    model: "haiku",
    condition: "control-cto-off",
    reasoning: "high",
    cto: "off",
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
    qualityScore: 75,
    ...overrides,
  };
}

vi.mock("../lib/api", () => ({
  fetchResults: vi.fn(),
}));

import * as api from "../lib/api";

describe("Matrix page", () => {
  beforeEach(() => {
    vi.mocked(api.fetchResults).mockReset();
  });

  it("renders a column per condition and a row per task config", async () => {
    vi.mocked(api.fetchResults).mockResolvedValue([
      row({ id: "1", condition: "control-cto-off", qualityScore: 70 }),
      row({ id: "2", condition: "explore", qualityScore: 85 }),
      row({ id: "3", condition: "leverage", qualityScore: 90 }),
    ]);
    render(<MemoryRouter><Matrix /></MemoryRouter>);
    await waitFor(() => {
      expect(screen.getByText(/bug-fix · grc · haiku/)).toBeInTheDocument();
    });
    // Condition column headers present
    expect(screen.getByText("control-cto-off")).toBeInTheDocument();
    expect(screen.getByText("explore")).toBeInTheDocument();
    expect(screen.getByText("leverage")).toBeInTheDocument();
    // Values visible
    expect(screen.getByText("70.0")).toBeInTheDocument();
    expect(screen.getByText("85.0")).toBeInTheDocument();
    expect(screen.getByText("90.0")).toBeInTheDocument();
  });

  it("renders an empty state when no results match", async () => {
    vi.mocked(api.fetchResults).mockResolvedValue([]);
    render(<MemoryRouter><Matrix /></MemoryRouter>);
    await waitFor(() => {
      expect(screen.getByText(/No runs match/)).toBeInTheDocument();
    });
  });

  it("shows an error when the API fails", async () => {
    vi.mocked(api.fetchResults).mockRejectedValue(new Error("boom"));
    render(<MemoryRouter><Matrix /></MemoryRouter>);
    await waitFor(() => {
      expect(screen.getByText(/Failed to load results/)).toBeInTheDocument();
    });
    expect(screen.getByText("boom")).toBeInTheDocument();
  });

  it("groups rows that share (evalType, target, model) under the same group header", async () => {
    vi.mocked(api.fetchResults).mockResolvedValue([
      row({ id: "1", reasoning: "high", condition: "control-cto-off", qualityScore: 60 }),
      row({ id: "2", reasoning: "low", condition: "control-cto-off", qualityScore: 50 }),
    ]);
    render(<MemoryRouter><Matrix /></MemoryRouter>);
    await waitFor(() => {
      expect(screen.getByText(/bug-fix · grc · haiku/)).toBeInTheDocument();
    });
    // Both reasoning levels rendered (as separate row labels), under one group.
    expect(screen.getAllByText(/bug-fix · grc · haiku/)).toHaveLength(1);
  });
});
