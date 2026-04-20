import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router-dom";
import Results from "./Results";
import type { EvalResult } from "../lib/types";

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

vi.mock("../lib/api", () => ({ fetchResults: vi.fn() }));

import * as api from "../lib/api";

describe("Results page — scorer-disagreement queue", () => {
  beforeEach(() => {
    vi.mocked(api.fetchResults).mockReset();
  });

  it("shows the divergent count next to the toggle when divergent rows exist", async () => {
    vi.mocked(api.fetchResults).mockResolvedValue([
      row({ id: "a", scorerAgreementDivergent: true, scorerAgreementGap: 25 }),
      row({ id: "b", scorerAgreementDivergent: false, scorerAgreementGap: 3 }),
      row({ id: "c", scorerAgreementDivergent: true, scorerAgreementGap: 18 }),
    ]);
    render(<MemoryRouter><Results /></MemoryRouter>);
    await waitFor(() => {
      // The toggle label is "Divergent only (2)"
      expect(screen.getByText(/Divergent only/)).toBeInTheDocument();
    });
    expect(screen.getByText("(2)")).toBeInTheDocument();
  });

  it("filters to only divergent rows when the toggle is enabled", async () => {
    vi.mocked(api.fetchResults).mockResolvedValue([
      row({ id: "a", scorerAgreementDivergent: true, scorerAgreementGap: 25 }),
      row({ id: "b", scorerAgreementDivergent: false, scorerAgreementGap: 3 }),
    ]);
    render(<MemoryRouter><Results /></MemoryRouter>);
    await waitFor(() => {
      expect(screen.getByText("2 results")).toBeInTheDocument();
    });
    const user = userEvent.setup();
    await user.click(screen.getByLabelText(/Divergent only/i));
    await waitFor(() => {
      expect(screen.getByText("1 result")).toBeInTheDocument();
    });
    // Banner appears
    expect(screen.getByText(/Review queue\./i)).toBeInTheDocument();
  });

  it("does not show the count when zero divergent rows", async () => {
    vi.mocked(api.fetchResults).mockResolvedValue([
      row({ id: "a", scorerAgreementDivergent: false }),
      row({ id: "b", scorerAgreementDivergent: null }),
    ]);
    render(<MemoryRouter><Results /></MemoryRouter>);
    await waitFor(() => {
      expect(screen.getByText(/Divergent only/)).toBeInTheDocument();
    });
    // No "(0)" displayed
    expect(screen.queryByText("(0)")).toBeNull();
  });
});
