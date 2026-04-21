import { useEffect, useMemo, useState } from "react";
import { fetchResults } from "../lib/api";
import type { EvalResult, EvalType, TargetName, ModelName } from "../lib/types";
import Sparkline, { type SparklinePoint } from "../components/charts/Sparkline";
import { conditionColor } from "../lib/chartData";
import { sortConditions } from "../lib/pivot";

const ALL = "__all__";
type Metric = "qualityScore" | "judgeScore" | "cost" | "totalTokens";

const METRIC_LABELS: Record<Metric, string> = {
  qualityScore: "Quality score",
  judgeScore: "Judge score",
  cost: "Cost (USD)",
  totalTokens: "Total tokens",
};

function fmtMetric(m: Metric, v: number): string {
  switch (m) {
    case "cost":        return `$${v.toFixed(2)}`;
    case "totalTokens": return formatTokens(v);
    default:            return Number.isInteger(v) ? String(v) : v.toFixed(1);
  }
}

function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(0)}K`;
  return String(Math.round(n));
}

interface TrendGroup {
  evalType: string;
  target: string;
  model: string;
  /** Condition → sparkline points ordered by date. */
  byCondition: Map<string, SparklinePoint[]>;
}

export default function Trends() {
  const [results, setResults] = useState<EvalResult[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [evalTypeFilter, setEvalTypeFilter] = useState<string>(ALL);
  const [targetFilter, setTargetFilter] = useState<string>(ALL);
  const [modelFilter, setModelFilter] = useState<string>(ALL);
  const [metric, setMetric] = useState<Metric>("qualityScore");

  useEffect(() => {
    fetchResults()
      .then((data) => { setResults(data); setLoading(false); })
      .catch((err) => { setError(err.message); setLoading(false); });
  }, []);

  const filtered = useMemo(() => {
    return results.filter((r) => {
      if (evalTypeFilter !== ALL && r.evalType !== evalTypeFilter) return false;
      if (targetFilter !== ALL && r.target !== targetFilter) return false;
      if (modelFilter !== ALL && r.model !== modelFilter) return false;
      return true;
    });
  }, [results, evalTypeFilter, targetFilter, modelFilter]);

  // Group (evalType × target × model) × condition → chronologically-sorted points.
  const groups = useMemo<TrendGroup[]>(() => {
    const buckets = new Map<string, TrendGroup>();
    for (const r of filtered) {
      const value = r[metric];
      if (typeof value !== "number") continue;
      const gKey = `${r.evalType}|${r.target}|${r.model}`;
      let g = buckets.get(gKey);
      if (!g) {
        g = {
          evalType: r.evalType,
          target: r.target,
          model: r.model,
          byCondition: new Map(),
        };
        buckets.set(gKey, g);
      }
      let pts = g.byCondition.get(r.condition);
      if (!pts) {
        pts = [];
        g.byCondition.set(r.condition, pts);
      }
      pts.push({ date: r.date, value, label: r.batchId ?? undefined });
    }
    // Sort each condition's points chronologically; drop groups with < 2 points total.
    const result: TrendGroup[] = [];
    for (const g of buckets.values()) {
      let totalPts = 0;
      for (const pts of g.byCondition.values()) {
        pts.sort((a, b) => a.date.localeCompare(b.date));
        totalPts += pts.length;
      }
      if (totalPts >= 2) result.push(g);
    }
    result.sort((a, b) => {
      const aKey = `${a.evalType}|${a.target}|${a.model}`;
      const bKey = `${b.evalType}|${b.target}|${b.model}`;
      return aKey.localeCompare(bKey);
    });
    return result;
  }, [filtered, metric]);

  const evalTypes: EvalType[] = [
    "bug-fix", "bug-fix-1", "explain-repo", "navigation-ctf", "cross-package",
    "impact-analysis", "feature-localization", "config-audit", "dead-code", "migration",
  ];
  const targets: TargetName[] = ["grc", "mediawiki"];
  const models: ModelName[] = ["haiku", "sonnet", "opus", "gpt-5.4"];

  if (loading) {
    return <div className="text-center py-12 text-[var(--color-text-muted)]">Loading results…</div>;
  }
  if (error) {
    return (
      <div className="text-center py-12 text-[var(--color-score-red)]">
        <p className="text-lg mb-2">Failed to load results</p>
        <p className="text-sm font-mono">{error}</p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-3">
        <div className="flex items-center gap-2">
          <label className="text-xs text-[var(--color-text-muted)] uppercase tracking-wide">Eval Type</label>
          <select
            value={evalTypeFilter}
            onChange={(e) => setEvalTypeFilter(e.target.value)}
            className="bg-[var(--color-surface)] border border-[var(--color-border)] rounded px-2.5 py-1.5 text-sm text-[var(--color-text)]"
          >
            <option value={ALL}>All</option>
            {evalTypes.map((t) => <option key={t} value={t}>{t}</option>)}
          </select>
        </div>
        <div className="flex items-center gap-2">
          <label className="text-xs text-[var(--color-text-muted)] uppercase tracking-wide">Target</label>
          <select
            value={targetFilter}
            onChange={(e) => setTargetFilter(e.target.value)}
            className="bg-[var(--color-surface)] border border-[var(--color-border)] rounded px-2.5 py-1.5 text-sm text-[var(--color-text)]"
          >
            <option value={ALL}>All</option>
            {targets.map((t) => <option key={t} value={t}>{t}</option>)}
          </select>
        </div>
        <div className="flex items-center gap-2">
          <label className="text-xs text-[var(--color-text-muted)] uppercase tracking-wide">Model</label>
          <select
            value={modelFilter}
            onChange={(e) => setModelFilter(e.target.value)}
            className="bg-[var(--color-surface)] border border-[var(--color-border)] rounded px-2.5 py-1.5 text-sm text-[var(--color-text)]"
          >
            <option value={ALL}>All</option>
            {models.map((m) => <option key={m} value={m}>{m}</option>)}
          </select>
        </div>
        <div className="flex-1" />
        <div className="flex items-center gap-2">
          <label className="text-xs text-[var(--color-text-muted)] uppercase tracking-wide">Metric</label>
          <select
            value={metric}
            onChange={(e) => setMetric(e.target.value as Metric)}
            className="bg-[var(--color-surface)] border border-[var(--color-border)] rounded px-2.5 py-1.5 text-sm text-[var(--color-text)]"
          >
            {(Object.keys(METRIC_LABELS) as Metric[]).map((m) => (
              <option key={m} value={m}>{METRIC_LABELS[m]}</option>
            ))}
          </select>
        </div>
      </div>

      {groups.length === 0 ? (
        <div className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface)] p-8 text-center text-sm text-[var(--color-text-muted)]">
          No configs with ≥2 historical points match the current filters. Run
          a few more evals for a config before expecting a trend.
        </div>
      ) : (
        <div className="grid grid-cols-1 lg:grid-cols-2 gap-3">
          {groups.map((g) => (
            <TrendCard key={`${g.evalType}|${g.target}|${g.model}`} group={g} metric={metric} />
          ))}
        </div>
      )}
    </div>
  );
}

function TrendCard({ group, metric }: { group: TrendGroup; metric: Metric }) {
  const conditions = sortConditions(Array.from(group.byCondition.keys()));
  const controlKey = conditions.find((c) => c === "control-cto-off")
    ?? conditions.find((c) => c.startsWith("control"))
    ?? null;
  const controlLatest = controlKey
    ? group.byCondition.get(controlKey)?.slice(-1)[0]?.value ?? undefined
    : undefined;

  return (
    <div className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface)] p-3 space-y-2">
      <div className="flex items-center justify-between">
        <div className="text-xs font-mono text-[var(--color-text-muted)]">
          {group.evalType} · {group.target} · {group.model}
        </div>
      </div>
      <div className="space-y-1.5">
        {conditions.map((cond) => {
          const pts = group.byCondition.get(cond) ?? [];
          if (pts.length === 0) return null;
          const color = conditionColor(cond);
          const latest = pts[pts.length - 1];
          return (
            <div key={cond} className="grid grid-cols-[8rem_1fr_5rem] gap-2 items-center text-xs">
              <span className="font-mono text-[var(--color-text)] truncate">
                <span
                  className="inline-block w-1.5 h-1.5 rounded-full mr-1.5"
                  style={{ background: color }}
                />
                {cond}
              </span>
              <div style={{ color }}>
                <Sparkline
                  points={pts}
                  color={color}
                  formatValue={(v) => fmtMetric(metric, v)}
                  target={cond === controlKey ? undefined : controlLatest}
                  width={240}
                  height={28}
                />
              </div>
              <span className="font-mono text-right text-[var(--color-text-muted)]">
                {fmtMetric(metric, latest.value)}
                <span className="ml-1 text-[10px]">({pts.length})</span>
              </span>
            </div>
          );
        })}
      </div>
    </div>
  );
}
