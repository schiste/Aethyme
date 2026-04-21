import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderHook, act, waitFor } from "@testing-library/react";
import { useLiveTabs, type LiveTab } from "./useLiveTabs";

function tab(overrides: Partial<LiveTab> = {}): LiveTab {
  return {
    tab_id: "tab_1",
    title: "Tab 1",
    window_id: 0,
    active_app: "shell",
    process_running: false,
    ...overrides,
  };
}

describe("useLiveTabs", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("does not fetch while disabled", () => {
    const fetcher = vi.fn().mockResolvedValue([tab()]);
    renderHook(() => useLiveTabs(false, { fetcher, intervalMs: 500 }));
    expect(fetcher).not.toHaveBeenCalled();
  });

  it("fetches once immediately on enable and updates state", async () => {
    const fetcher = vi.fn().mockResolvedValue([tab({ tab_id: "tab_7" })]);
    const { result } = renderHook(() =>
      useLiveTabs(true, { fetcher, intervalMs: 1000 }),
    );
    await vi.waitFor(() => {
      expect(result.current.tabs).toHaveLength(1);
    });
    expect(result.current.tabs?.[0].tab_id).toBe("tab_7");
    expect(result.current.lastUpdatedAt).not.toBeNull();
    expect(fetcher).toHaveBeenCalledTimes(1);
  });

  it("polls on the configured interval", async () => {
    const fetcher = vi.fn().mockResolvedValue([tab()]);
    renderHook(() => useLiveTabs(true, { fetcher, intervalMs: 2000 }));
    await vi.waitFor(() => expect(fetcher).toHaveBeenCalledTimes(1));
    await act(async () => { vi.advanceTimersByTime(2000); });
    await vi.waitFor(() => expect(fetcher).toHaveBeenCalledTimes(2));
    await act(async () => { vi.advanceTimersByTime(2000); });
    await vi.waitFor(() => expect(fetcher).toHaveBeenCalledTimes(3));
  });

  it("stops polling when disabled is flipped back to false", async () => {
    const fetcher = vi.fn().mockResolvedValue([tab()]);
    const { rerender } = renderHook(
      ({ enabled }: { enabled: boolean }) =>
        useLiveTabs(enabled, { fetcher, intervalMs: 1000 }),
      { initialProps: { enabled: true } },
    );
    await vi.waitFor(() => expect(fetcher).toHaveBeenCalledTimes(1));
    rerender({ enabled: false });
    await act(async () => { vi.advanceTimersByTime(5000); });
    expect(fetcher).toHaveBeenCalledTimes(1);
  });

  it("preserves last tabs when a later fetch errors", async () => {
    const fetcher = vi.fn()
      .mockResolvedValueOnce([tab({ tab_id: "tab_good" })])
      .mockRejectedValueOnce(new Error("network down"));
    const { result } = renderHook(() =>
      useLiveTabs(true, { fetcher, intervalMs: 1000 }),
    );
    await vi.waitFor(() => expect(result.current.tabs).toHaveLength(1));
    await act(async () => { vi.advanceTimersByTime(1000); });
    await vi.waitFor(() => expect(result.current.error).toBe("network down"));
    // Tabs preserved despite the error
    expect(result.current.tabs?.[0].tab_id).toBe("tab_good");
  });

  it("tears down cleanly on unmount — no fetches after", async () => {
    const fetcher = vi.fn().mockResolvedValue([tab()]);
    const { unmount } = renderHook(() =>
      useLiveTabs(true, { fetcher, intervalMs: 500 }),
    );
    await vi.waitFor(() => expect(fetcher).toHaveBeenCalledTimes(1));
    unmount();
    await act(async () => { vi.advanceTimersByTime(5000); });
    expect(fetcher).toHaveBeenCalledTimes(1);
  });
});

// Silence the test-only `waitFor` unused-import warning; ensures tree-shake
// doesn't yell if a future version of vitest tightens imports.
void waitFor;
