import { useEffect, useMemo, useState } from "react";
import { fetchResults } from "../lib/api";
import type { EvalResult, EvalType, TargetName, ModelName } from "../lib/types";
import { buildParetoPoints } from "../lib/chartData";
import Scatter from "../components/charts/Scatter";

const ALL = "__all__";

type XAxis = "cost" | "totalTokens" | "durationSeconds";
type YAxis = "qualityScore" | "judgeScore";

const X_AXES: { value: XAxis; label: string; format: (v: number) => string }[] = [
  { value: "cost",            label: "Cost (USD)",  format: (v) => `$${v.toFixed(2)}` },
  { value: "totalTokens",     label: "Total tokens", format: formatTokens },
  { value: "durationSeconds", label: "Duration (s)", format: (v) => `${v.toFixed(0)}s` },
];

const Y_AXES: { value: YAxis; label: string }[] = [
  { value: "qualityScore", label: "Quality score" },
  { value: "judgeScore",   label: "Judge score"   },
];

function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(0)}K`;
  return String(Math.round(n));
}

export default function Charts() {
  const [results, setResults] = useState<EvalResult[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const [evalTypeFilter, setEvalTypeFilter] = useState<string>(ALL);
  const [targetFilter, setTargetFilter] = useState<string>(ALL);
  const [modelFilter, setModelFilter] = useState<string>(ALL);
  const [xAxis, setXAxis] = useState<XAxis>("cost");
  const [yAxis, setYAxis] = useState<YAxis>("qualityScore");

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

  const points = useMemo(() => buildParetoPoints(filtered), [filtered]);

  const xConfig = X_AXES.find((a) => a.value === xAxis)!;
  const yConfig = Y_AXES.find((a) => a.value === yAxis)!;

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
      {/* Filter/axis bar */}
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
          <label className="text-xs text-[var(--color-text-muted)] uppercase tracking-wide">Y</label>
          <select
            value={yAxis}
            onChange={(e) => setYAxis(e.target.value as YAxis)}
            className="bg-[var(--color-surface)] border border-[var(--color-border)] rounded px-2.5 py-1.5 text-sm text-[var(--color-text)]"
          >
            {Y_AXES.map((a) => <option key={a.value} value={a.value}>{a.label}</option>)}
          </select>
        </div>

        <div className="flex items-center gap-2">
          <label className="text-xs text-[var(--color-text-muted)] uppercase tracking-wide">X</label>
          <select
            value={xAxis}
            onChange={(e) => setXAxis(e.target.value as XAxis)}
            className="bg-[var(--color-surface)] border border-[var(--color-border)] rounded px-2.5 py-1.5 text-sm text-[var(--color-text)]"
          >
            {X_AXES.map((a) => <option key={a.value} value={a.value}>{a.label}</option>)}
          </select>
        </div>
      </div>

      <div className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface)] p-4">
        <div className="flex items-center justify-between mb-3">
          <h2 className="text-sm font-semibold text-[var(--color-text)]">
            {yConfig.label} vs {xConfig.label}
          </h2>
          <span className="text-xs text-[var(--color-text-muted)] font-mono">
            {points.length} point{points.length !== 1 ? "s" : ""}
          </span>
        </div>
        <p className="text-xs text-[var(--color-text-muted)] mb-3">
          One dot per batch-condition. Point size grows with repetition count.
          Vertical bar shows cross-run stdev where available. Dashed outline
          means at least one run failed its deliverable — treat the score
          with skepticism.
        </p>
        <Scatter
          points={points}
          xField={xAxis}
          xLabel={xConfig.label}
          formatX={xConfig.format}
          yField={yAxis}
          yLabel={yConfig.label}
        />
      </div>
    </div>
  );
}
