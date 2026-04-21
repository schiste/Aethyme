import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import ResultsTable from "./ResultsTable";
import type { EvalResult } from "../lib/types";

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
    cost: 0.1,
    duration: "30s",
    fixed: true,
    scenario: null,
    output: "default output",
    toolBreakdown: null,
    prompt: null,
    runId: "run-abc123",
    qualityScore: 80,
    ...overrides,
  };
}

describe("ResultsTable — diff selection flow", () => {
  it("shows the diff action bar once a row is selected and enables Compare at 2", async () => {
    const rows = [
      row({ id: "a", condition: "control-cto-off", output: "line 1\nline 2" }),
      row({ id: "b", condition: "explore", output: "line 1\nDIFFERENT" }),
      row({ id: "c", condition: "leverage", output: "line 1\nline 2" }),
    ];
    render(<ResultsTable results={rows} />);
    const checkboxes = screen.getAllByRole("checkbox", { name: /Select for diff/i });
    expect(checkboxes).toHaveLength(3);

    const user = userEvent.setup();
    await user.click(checkboxes[0]);
    // Action bar now visible with 1/2 selected, Compare button disabled.
    expect(screen.getByText(/1\/2 selected/)).toBeInTheDocument();
    const compareBtn = screen.getByRole("button", { name: /Compare outputs/i });
    expect(compareBtn).toBeDisabled();

    await user.click(checkboxes[1]);
    expect(screen.getByText(/2\/2 selected/)).toBeInTheDocument();
    expect(compareBtn).toBeEnabled();
  });

  it("caps selection at 2 — picking a third drops the first", async () => {
    const rows = [
      row({ id: "a", condition: "control-cto-off" }),
      row({ id: "b", condition: "explore" }),
      row({ id: "c", condition: "leverage" }),
    ];
    render(<ResultsTable results={rows} />);
    const checkboxes = screen.getAllByRole("checkbox", { name: /Select for diff/i });
    const user = userEvent.setup();
    await user.click(checkboxes[0]);
    await user.click(checkboxes[1]);
    await user.click(checkboxes[2]);
    // First (control-cto-off) was evicted — we should see explore + leverage tags
    // in the action bar; check by asserting the action bar has 2 chips and the
    // "control-cto-off" label is no longer among them.
    expect(screen.getByText(/2\/2 selected/)).toBeInTheDocument();
    // The condition chips appear both in the action bar and the table cells,
    // so we rely on presence/absence in the action bar's ✕ buttons.
    const removeBtns = screen.getAllByRole("button", { name: /Remove .* from diff selection/i });
    const labels = removeBtns.map((btn) => btn.getAttribute("aria-label"));
    expect(labels.some((l) => l?.includes("explore"))).toBe(true);
    expect(labels.some((l) => l?.includes("leverage"))).toBe(true);
    expect(labels.some((l) => l?.includes("control-cto-off"))).toBe(false);
  });

  it("clicking Compare opens the diff modal", async () => {
    const rows = [
      row({ id: "a", condition: "control-cto-off", output: "alpha\nbeta" }),
      row({ id: "b", condition: "explore",         output: "alpha\nGAMMA" }),
    ];
    render(<ResultsTable results={rows} />);
    const user = userEvent.setup();
    const checkboxes = screen.getAllByRole("checkbox", { name: /Select for diff/i });
    await user.click(checkboxes[0]);
    await user.click(checkboxes[1]);
    await user.click(screen.getByRole("button", { name: /Compare outputs/i }));
    // Modal header: "A: <cond> vs B: <cond>"
    expect(screen.getByText(/Similarity:/)).toBeInTheDocument();
    // 50% because 1/2 lines match
    expect(screen.getByText("50%")).toBeInTheDocument();
  });
});
