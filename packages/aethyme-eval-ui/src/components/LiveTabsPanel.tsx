import { useLiveTabs, type LiveTab } from "../lib/useLiveTabs";

interface Props {
  enabled: boolean;
  intervalMs?: number;
  /** Optional filter — typically the set of tab IDs the current run launched. */
  filterTabIds?: Set<string>;
}

function freshness(lastUpdatedAt: number | null): string {
  if (lastUpdatedAt === null) return "never";
  const ms = Date.now() - lastUpdatedAt;
  if (ms < 2000) return "just now";
  const s = Math.floor(ms / 1000);
  if (s < 60) return `${s}s ago`;
  return `${Math.floor(s / 60)}m ago`;
}

function StateDot({ tab }: { tab: LiveTab }) {
  const running = tab.process_running === true;
  const color = running ? "bg-[var(--color-accent)]" : "bg-[var(--color-border)]";
  return (
    <span
      className={`inline-block w-2 h-2 rounded-full ${color}`}
      title={running ? "Process running" : "Idle"}
    />
  );
}

function TabCard({ tab }: { tab: LiveTab }) {
  const app = tab.active_app ?? "shell";
  return (
    <div className="rounded border border-[var(--color-border)] bg-[var(--color-bg)] px-3 py-2 text-xs flex items-center gap-3 min-w-0">
      <StateDot tab={tab} />
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2 truncate">
          <span className="font-mono text-[var(--color-text)] truncate">
            {tab.title ?? tab.tab_id}
          </span>
          {typeof tab.window_id === "number" && (
            <span className="text-[10px] font-mono text-[var(--color-text-muted)]">
              w{tab.window_id}
            </span>
          )}
        </div>
        <div className="text-[10px] font-mono text-[var(--color-text-muted)] truncate">
          {tab.tab_id} · {app}
        </div>
      </div>
    </div>
  );
}

export default function LiveTabsPanel({ enabled, intervalMs = 2000, filterTabIds }: Props) {
  const { tabs, loading, error, lastUpdatedAt } = useLiveTabs(enabled, { intervalMs });

  if (!enabled) return null;

  const visible = tabs
    ? filterTabIds
      ? tabs.filter((t) => filterTabIds.has(t.tab_id))
      : tabs
    : null;

  return (
    <div className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface)] p-4 space-y-3">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <span
            className={`inline-block w-1.5 h-1.5 rounded-full ${loading ? "bg-[var(--color-accent)] animate-pulse" : "bg-[var(--color-score-green)]"}`}
          />
          <span className="text-sm font-semibold text-[var(--color-text)]">
            Live Chau7 tabs
          </span>
          <span className="text-xs text-[var(--color-text-muted)]">
            polling every {(intervalMs / 1000).toFixed(0)}s · {freshness(lastUpdatedAt)}
          </span>
        </div>
        {error && (
          <span className="text-xs text-[var(--color-score-red)] font-mono" title={error}>
            error: {error}
          </span>
        )}
      </div>

      {visible === null ? (
        <div className="text-xs text-[var(--color-text-muted)]">Fetching tab list…</div>
      ) : visible.length === 0 ? (
        <div className="text-xs text-[var(--color-text-muted)]">
          No tabs {filterTabIds ? "match this run" : "open"}.
        </div>
      ) : (
        <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-2">
          {visible.map((tab) => (
            <TabCard key={tab.tab_id} tab={tab} />
          ))}
        </div>
      )}
    </div>
  );
}
