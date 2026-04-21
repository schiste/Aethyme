import { useEffect, useRef, useState } from "react";

// ---------------------------------------------------------------------------
// useLiveTabs — poll /api/chau7/tabs on an interval while enabled.
//
// Enabled is the discriminator: we start polling on the false→true transition
// and tear down on true→false. Crucially we also guard the fetcher with a
// mounted flag so a late-arriving response from a previous cycle can't
// overwrite fresher state after the user navigates away.
//
// The default 2000ms cadence is chosen to stay responsive without spamming
// the Chau7 MCP — tab state (active app, process_running) typically changes
// on human timescales, not sub-second.
// ---------------------------------------------------------------------------

export interface LiveTab {
  tab_id: string;
  window_id?: number;
  title?: string;
  working_directory?: string;
  process_running?: boolean;
  active_app?: string;
  [k: string]: unknown;
}

export interface LiveTabsState {
  tabs: LiveTab[] | null;
  loading: boolean;
  error: string | null;
  lastUpdatedAt: number | null;
}

export type Fetcher = () => Promise<LiveTab[]>;

export function useLiveTabs(
  enabled: boolean,
  opts: { intervalMs?: number; fetcher?: Fetcher } = {},
): LiveTabsState {
  const { intervalMs = 2000 } = opts;
  const fetcher = opts.fetcher;
  const [state, setState] = useState<LiveTabsState>({
    tabs: null, loading: false, error: null, lastUpdatedAt: null,
  });
  const mountedRef = useRef(true);

  useEffect(() => {
    mountedRef.current = true;
    return () => { mountedRef.current = false; };
  }, []);

  useEffect(() => {
    if (!enabled) return;

    let cancelled = false;

    async function tick() {
      if (cancelled || !mountedRef.current) return;
      setState((prev) => ({ ...prev, loading: true }));
      try {
        const tabs = fetcher
          ? await fetcher()
          : await fetch("/api/chau7/tabs").then(async (res) => {
              if (!res.ok) throw new Error(`HTTP ${res.status}`);
              return (await res.json()) as LiveTab[];
            });
        if (cancelled || !mountedRef.current) return;
        setState({
          tabs,
          loading: false,
          error: null,
          lastUpdatedAt: Date.now(),
        });
      } catch (err) {
        if (cancelled || !mountedRef.current) return;
        setState((prev) => ({
          tabs: prev.tabs,
          loading: false,
          error: err instanceof Error ? err.message : String(err),
          lastUpdatedAt: prev.lastUpdatedAt,
        }));
      }
    }

    tick();
    const id = setInterval(tick, intervalMs);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [enabled, intervalMs, fetcher]);

  return state;
}
