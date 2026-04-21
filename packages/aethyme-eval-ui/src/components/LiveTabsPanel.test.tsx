import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen } from "@testing-library/react";
import LiveTabsPanel from "./LiveTabsPanel";

// Mock the hook directly so we don't have to drive timers for component tests.
vi.mock("../lib/useLiveTabs", () => ({
  useLiveTabs: vi.fn(),
}));

import { useLiveTabs } from "../lib/useLiveTabs";

describe("LiveTabsPanel", () => {
  beforeEach(() => {
    vi.mocked(useLiveTabs).mockReset();
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("renders nothing when disabled (doesn't even call the hook for data)", () => {
    vi.mocked(useLiveTabs).mockReturnValue({
      tabs: null, loading: false, error: null, lastUpdatedAt: null,
    });
    const { container } = render(<LiveTabsPanel enabled={false} />);
    expect(container.textContent).toBe("");
  });

  it("renders the initial fetching state while tabs is null", () => {
    vi.mocked(useLiveTabs).mockReturnValue({
      tabs: null, loading: true, error: null, lastUpdatedAt: null,
    });
    render(<LiveTabsPanel enabled={true} />);
    expect(screen.getByText(/Fetching tab list/i)).toBeInTheDocument();
  });

  it("renders one card per tab and surfaces tab metadata", () => {
    vi.mocked(useLiveTabs).mockReturnValue({
      tabs: [
        { tab_id: "tab_A", title: "control", window_id: 0, active_app: "Claude", process_running: true },
        { tab_id: "tab_B", title: "explore", window_id: 1, active_app: "Codex",  process_running: false },
      ],
      loading: false,
      error: null,
      lastUpdatedAt: Date.now(),
    });
    render(<LiveTabsPanel enabled={true} />);
    expect(screen.getByText("control")).toBeInTheDocument();
    expect(screen.getByText("explore")).toBeInTheDocument();
    expect(screen.getByText(/tab_A · Claude/)).toBeInTheDocument();
    expect(screen.getByText(/tab_B · Codex/)).toBeInTheDocument();
  });

  it("filters to a provided set of tab IDs", () => {
    vi.mocked(useLiveTabs).mockReturnValue({
      tabs: [
        { tab_id: "tab_A", title: "alpha" },
        { tab_id: "tab_B", title: "beta"  },
        { tab_id: "tab_C", title: "gamma" },
      ],
      loading: false, error: null, lastUpdatedAt: Date.now(),
    });
    render(
      <LiveTabsPanel enabled={true} filterTabIds={new Set(["tab_A", "tab_C"])} />,
    );
    expect(screen.getByText("alpha")).toBeInTheDocument();
    expect(screen.getByText("gamma")).toBeInTheDocument();
    expect(screen.queryByText("beta")).toBeNull();
  });

  it("shows the error text when the hook reports an error, while keeping cached tabs", () => {
    vi.mocked(useLiveTabs).mockReturnValue({
      tabs: [{ tab_id: "tab_A", title: "still here" }],
      loading: false,
      error: "network down",
      lastUpdatedAt: Date.now() - 10_000,
    });
    render(<LiveTabsPanel enabled={true} />);
    expect(screen.getByText(/error: network down/)).toBeInTheDocument();
    expect(screen.getByText("still here")).toBeInTheDocument();
  });
});
