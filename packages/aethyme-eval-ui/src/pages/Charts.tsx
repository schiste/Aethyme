import { useEffect, useMemo, useState } from "react";
import { fetchResults } from "../lib/api";
import type { EvalResult, EvalType, TargetName, ModelName } from "../lib/types";
import { buildParetoPoints } from "../lib/chartData";
import Scatter from "../components/charts/Scatter";
import BoxPlot, { type BoxGroup } from "../components/charts/BoxPlot";
import Heatmap, { type HeatmapCell } from "../components/charts/Heatmap";
import { sortConditions } from "../lib/pivot";

const ALL = "__all__";

type Tab = "pareto" | "distributions" | "heatmap";
type HeatmapAxis = "reasoning" | "model" | "target" | "evalType";
type HeatmapMetric = "qualityScore" | "judgeScore" | "cost" | "totalTokens";
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

  const [tab, setTab] = useState<Tab>("pareto");
  const [evalTypeFilter, setEvalTypeFilter] = useState<string>(ALL);
  const [targetFilter, setTargetFilter] = useState<string>(ALL);
  const [modelFilter, setModelFilter] = useState<string>(ALL);
  const [xAxis, setXAxis] = useState<XAxis>("cost");
  const [yAxis, setYAxis] = useState<YAxis>("qualityScore");
  const [distMetric, setDistMetric] = useState<"qualityScore" | "judgeScore" | "cost" | "totalTokens">("qualityScore");
  const [distBatchKey, setDistBatchKey] = useState<string | null>(null);
  const [heatmapRowField, setHeatmapRowField] = useState<HeatmapAxis>("reasoning");
  const [heatmapMetric, setHeatmapMetric] = useState<HeatmapMetric>("qualityScore");

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

  // Distribution data: group the filtered rows by batch, then by condition.
  // A box plot is only meaningful when you look at a single batch so the
  // variation is "same config, different runs" — not mixing batches.
  // Declared before any early-return so hook order stays stable.
  const batchOptions = useMemo(() => {
    const seen = new Map<string, { label: string; rows: EvalResult[] }>();
    for (const r of filtered) {
      const key = [
        r.evalType, r.target, r.model, r.reasoning,
        r.scenario ?? "—", r.batchId ?? `single:${r.id}`,
      ].join("|");
      const existing = seen.get(key);
      if (existing) existing.rows.push(r);
      else seen.set(key, {
        label: [
          r.evalType, r.target, r.model,
          r.reasoning,
          r.scenario ? `/${r.scenario}` : "",
          r.batchId ? `· ${r.batchId.slice(0, 8)}` : "· single",
        ].filter(Boolean).join(" "),
        rows: [r],
      });
    }
    return Array.from(seen.entries())
      .map(([key, b]) => ({ key, ...b }))
      .filter((b) => {
        const perCond = new Map<string, number>();
        for (const r of b.rows) perCond.set(r.condition, (perCond.get(r.condition) ?? 0) + 1);
        return Array.from(perCond.values()).some((n) => n >= 2);
      });
  }, [filtered]);

  const activeBatch =
    batchOptions.find((b) => b.key === distBatchKey) ?? batchOptions[0];

  const boxGroups = useMemo<BoxGroup[]>(() => {
    if (!activeBatch) return [];
    const byCond = new Map<string, EvalResult[]>();
    for (const r of activeBatch.rows) {
      const bucket = byCond.get(r.condition);
      if (bucket) bucket.push(r);
      else byCond.set(r.condition, [r]);
    }
    const ordered = sortConditions(Array.from(byCond.keys()));
    return ordered.map((cond) => ({
      label: cond,
      values: (byCond.get(cond) ?? []).map((r) => r[distMetric] ?? null),
    }));
  }, [activeBatch, distMetric]);

  // Heatmap data: pivot by (rowField) × condition, with mean of `heatmapMetric`.
  // Empty cells (no runs for this row × condition) render as "—".
  const heatmap = useMemo(() => {
    const rowVals = new Set<string>();
    const colVals = new Set<string>();
    const bucket = new Map<string, number[]>();
    const countBucket = new Map<string, number>();
    for (const r of filtered) {
      const rowKey = String(r[heatmapRowField] ?? "—");
      const colKey = r.condition;
      rowVals.add(rowKey);
      colVals.add(colKey);
      const v = r[heatmapMetric];
      if (typeof v === "number" && Number.isFinite(v)) {
        const key = `${rowKey}|${colKey}`;
        const arr = bucket.get(key);
        if (arr) arr.push(v);
        else bucket.set(key, [v]);
        countBucket.set(key, (countBucket.get(key) ?? 0) + 1);
      }
    }
    const orderedRows = Array.from(rowVals).sort();
    const orderedCols = sortConditions(Array.from(colVals));
    const cells: HeatmapCell[] = [];
    for (const row of orderedRows) {
      for (const col of orderedCols) {
        const key = `${row}|${col}`;
        const arr = bucket.get(key);
        if (arr && arr.length > 0) {
          const mean = arr.reduce((s, n) => s + n, 0) / arr.length;
          cells.push({ row, column: col, value: mean, n: arr.length });
        } else {
          cells.push({ row, column: col, value: null });
        }
      }
    }
    return { rows: orderedRows, columns: orderedCols, cells };
  }, [filtered, heatmapRowField, heatmapMetric]);

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
      {/* Tab bar */}
      <div className="flex gap-1 border-b border-[var(--color-border)]">
        {(["pareto", "distributions", "heatmap"] as Tab[]).map((t) => (
          <button
            key={t}
            onClick={() => setTab(t)}
            className={[
              "px-3 py-1.5 text-sm rounded-t transition-colors",
              tab === t
                ? "text-[var(--color-text)] border-b-2 border-[var(--color-accent)] -mb-px"
                : "text-[var(--color-text-muted)] hover:text-[var(--color-text)]",
            ].join(" ")}
          >
            {t === "pareto" ? "Pareto scatter" : t === "distributions" ? "Distributions" : "Heatmap"}
          </button>
        ))}
      </div>

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

        {tab === "pareto" && (
          <>
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
          </>
        )}

        {tab === "distributions" && (
          <div className="flex items-center gap-2">
            <label className="text-xs text-[var(--color-text-muted)] uppercase tracking-wide">Metric</label>
            <select
              value={distMetric}
              onChange={(e) => setDistMetric(e.target.value as typeof distMetric)}
              className="bg-[var(--color-surface)] border border-[var(--color-border)] rounded px-2.5 py-1.5 text-sm text-[var(--color-text)]"
            >
              <option value="qualityScore">Quality</option>
              <option value="judgeScore">Judge</option>
              <option value="cost">Cost</option>
              <option value="totalTokens">Total tokens</option>
            </select>
          </div>
        )}

        {tab === "heatmap" && (
          <>
            <div className="flex items-center gap-2">
              <label className="text-xs text-[var(--color-text-muted)] uppercase tracking-wide">Rows</label>
              <select
                value={heatmapRowField}
                onChange={(e) => setHeatmapRowField(e.target.value as HeatmapAxis)}
                className="bg-[var(--color-surface)] border border-[var(--color-border)] rounded px-2.5 py-1.5 text-sm text-[var(--color-text)]"
              >
                <option value="reasoning">Reasoning</option>
                <option value="model">Model</option>
                <option value="target">Target</option>
                <option value="evalType">Eval type</option>
              </select>
            </div>
            <div className="flex items-center gap-2">
              <label className="text-xs text-[var(--color-text-muted)] uppercase tracking-wide">Metric</label>
              <select
                value={heatmapMetric}
                onChange={(e) => setHeatmapMetric(e.target.value as HeatmapMetric)}
                className="bg-[var(--color-surface)] border border-[var(--color-border)] rounded px-2.5 py-1.5 text-sm text-[var(--color-text)]"
              >
                <option value="qualityScore">Quality</option>
                <option value="judgeScore">Judge</option>
                <option value="cost">Cost</option>
                <option value="totalTokens">Total tokens</option>
              </select>
            </div>
          </>
        )}
      </div>

      {tab === "pareto" && (
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
      )}

      {tab === "heatmap" && (
        <div className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface)] p-4 space-y-3">
          <div className="flex items-center justify-between gap-3 flex-wrap">
            <h2 className="text-sm font-semibold text-[var(--color-text)]">
              {heatmapRowLabel(heatmapRowField)} × condition — {distMetricLabel(heatmapMetric)}
            </h2>
            <span className="text-xs text-[var(--color-text-muted)] font-mono">
              {heatmap.rows.length} × {heatmap.columns.length}
            </span>
          </div>
          <p className="text-xs text-[var(--color-text-muted)]">
            Cell value is the mean of {distMetricLabel(heatmapMetric)} across all
            runs matching that row × column. Pick "Reasoning" as the row axis
            to visualize the reasoning × navigation interaction — navigation
            tools tend to help most when reasoning is constrained.
          </p>
          {heatmap.rows.length === 0 ? (
            <div className="text-center py-12 text-[var(--color-text-muted)] text-sm">
              No rows match the current filters.
            </div>
          ) : (
            <Heatmap
              rows={heatmap.rows}
              columns={heatmap.columns}
              cells={heatmap.cells}
              higherIsBetter={heatmapMetric === "qualityScore" || heatmapMetric === "judgeScore"}
              formatValue={distMetricFormatter(heatmapMetric)}
            />
          )}
        </div>
      )}

      {tab === "distributions" && (
        <div className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface)] p-4 space-y-3">
          <div className="flex items-center justify-between gap-3 flex-wrap">
            <h2 className="text-sm font-semibold text-[var(--color-text)]">
              Distribution of {distMetricLabel(distMetric)} across conditions
            </h2>
            {batchOptions.length > 0 && (
              <select
                value={activeBatch?.key ?? ""}
                onChange={(e) => setDistBatchKey(e.target.value)}
                className="bg-[var(--color-surface)] border border-[var(--color-border)] rounded px-2.5 py-1.5 text-xs text-[var(--color-text)] max-w-full"
              >
                {batchOptions.map((b) => (
                  <option key={b.key} value={b.key}>{b.label}</option>
                ))}
              </select>
            )}
          </div>
          {batchOptions.length === 0 ? (
            <div className="text-center py-12 text-[var(--color-text-muted)] text-sm">
              No batches with repetitions match the current filters. Box plots
              need at least 2 runs per condition to be meaningful.
            </div>
          ) : (
            <>
              <p className="text-xs text-[var(--color-text-muted)]">
                Box = interquartile range · line in box = median · whiskers =
                1.5×IQR · dots = outliers. Conditions with n&lt;2 runs show
                as a single highlighted dot.
              </p>
              <BoxPlot
                groups={boxGroups}
                yLabel={distMetricLabel(distMetric)}
                formatY={distMetricFormatter(distMetric)}
              />
            </>
          )}
        </div>
      )}
    </div>
  );
}

function distMetricLabel(m: string): string {
  switch (m) {
    case "qualityScore": return "Quality";
    case "judgeScore":   return "Judge";
    case "cost":         return "Cost (USD)";
    case "totalTokens":  return "Total tokens";
    default: return m;
  }
}

function distMetricFormatter(m: string): (v: number) => string {
  switch (m) {
    case "cost":        return (v) => `$${v.toFixed(2)}`;
    case "totalTokens": return formatTokens;
    default:            return (v) => (Number.isInteger(v) ? String(v) : v.toFixed(1));
  }
}

function heatmapRowLabel(field: string): string {
  switch (field) {
    case "reasoning": return "Reasoning";
    case "model":     return "Model";
    case "target":    return "Target";
    case "evalType":  return "Eval type";
    default:          return field;
  }
}
