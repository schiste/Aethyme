import { useEffect, useMemo, useState } from "react";
import { fetchResults } from "../lib/api";
import type { EvalResult, EvalType, TargetName, ModelName } from "../lib/types";
import {
  pivotResults,
  sortConditions,
  type MatrixCell,
  type MatrixRow,
} from "../lib/pivot";

const ALL = "__all__";

type MetricKey = "qualityScore" | "judgeScore" | "recalculatedEvalScore" | "totalTokens" | "cost";

const METRICS: { key: MetricKey; label: string; higherIsBetter: boolean; format: (v: number) => string }[] = [
  { key: "qualityScore", label: "Quality", higherIsBetter: true, format: (v) => v.toFixed(1) },
  { key: "judgeScore", label: "Judge", higherIsBetter: true, format: (v) => v.toFixed(1) },
  { key: "recalculatedEvalScore", label: "Recalc", higherIsBetter: true, format: (v) => v.toFixed(1) },
  { key: "totalTokens", label: "Tokens", higherIsBetter: false, format: formatTokens },
  { key: "cost", label: "Cost", higherIsBetter: false, format: (v) => `$${v.toFixed(2)}` },
];

function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(0)}K`;
  return String(n);
}

function scoreColor(score: number | null, higherIsBetter: boolean): string {
  if (score === null) return "text-[var(--color-text-muted)]";
  if (!higherIsBetter) return "text-[var(--color-text)]";
  if (score >= 80) return "text-[var(--color-score-green)]";
  if (score >= 50) return "text-[var(--color-score-yellow)]";
  return "text-[var(--color-score-red)]";
}

function DeliverableDot({ status }: { status: MatrixCell["deliverableStatus"] }) {
  if (status === null || status === undefined) return null;
  const color =
    status === "success" ? "bg-[var(--color-score-green)]" :
    status === "degraded" ? "bg-[var(--color-score-yellow)]" :
    "bg-[var(--color-score-red)]";
  return <span className={`inline-block w-1.5 h-1.5 rounded-full ${color}`} title={`Deliverable: ${status}`} />;
}

function CellView({ cell, metric }: { cell: MatrixCell | undefined; metric: (typeof METRICS)[number] }) {
  if (!cell) {
    return <span className="text-[var(--color-text-muted)] font-mono">—</span>;
  }
  const value = cell[metric.key];
  if (value === null) {
    return <span className="text-[var(--color-text-muted)] font-mono">—</span>;
  }
  const stdev = metric.key === "qualityScore" ? cell.qualityStdev : null;
  const n = cell.runs.length;
  return (
    <span className={`inline-flex items-center gap-1 font-mono ${scoreColor(value, metric.higherIsBetter)}`}>
      <DeliverableDot status={cell.deliverableStatus} />
      <span className="font-semibold">{metric.format(value)}</span>
      {typeof stdev === "number" && (
        <span className="text-[10px] text-[var(--color-text-muted)]">
          ±{stdev.toFixed(1)}
        </span>
      )}
      {n > 1 && (
        <span className="text-[10px] text-[var(--color-text-muted)]">
          ×{n}
        </span>
      )}
    </span>
  );
}

export default function Matrix() {
  const [results, setResults] = useState<EvalResult[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const [evalTypeFilter, setEvalTypeFilter] = useState<string>(ALL);
  const [targetFilter, setTargetFilter] = useState<string>(ALL);
  const [modelFilter, setModelFilter] = useState<string>(ALL);
  const [metric, setMetric] = useState<MetricKey>("qualityScore");

  useEffect(() => {
    fetchResults()
      .then((data) => {
        setResults(data);
        setLoading(false);
      })
      .catch((err) => {
        setError(err.message);
        setLoading(false);
      });
  }, []);

  const filtered = useMemo(() => {
    return results.filter((r) => {
      if (evalTypeFilter !== ALL && r.evalType !== evalTypeFilter) return false;
      if (targetFilter !== ALL && r.target !== targetFilter) return false;
      if (modelFilter !== ALL && r.model !== modelFilter) return false;
      return true;
    });
  }, [results, evalTypeFilter, targetFilter, modelFilter]);

  const matrix = useMemo(() => pivotResults(filtered), [filtered]);
  const orderedConditions = useMemo(
    () => sortConditions(matrix.conditions),
    [matrix.conditions],
  );

  const activeMetric = METRICS.find((m) => m.key === metric)!;

  const evalTypes: EvalType[] = [
    "bug-fix", "bug-fix-1", "explain-repo", "navigation-ctf", "cross-package",
    "impact-analysis", "feature-localization", "config-audit", "dead-code", "migration",
  ];
  const targets: TargetName[] = ["aethyme", "grc", "mediawiki"];
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
          <label className="text-xs text-[var(--color-text-muted)] uppercase tracking-wide">
            Eval Type
          </label>
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
          <label className="text-xs text-[var(--color-text-muted)] uppercase tracking-wide">
            Target
          </label>
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
          <label className="text-xs text-[var(--color-text-muted)] uppercase tracking-wide">
            Model
          </label>
          <select
            value={modelFilter}
            onChange={(e) => setModelFilter(e.target.value)}
            className="bg-[var(--color-surface)] border border-[var(--color-border)] rounded px-2.5 py-1.5 text-sm text-[var(--color-text)]"
          >
            <option value={ALL}>All</option>
            {models.map((m) => <option key={m} value={m}>{m}</option>)}
          </select>
        </div>

        <div className="flex items-center gap-2">
          <label className="text-xs text-[var(--color-text-muted)] uppercase tracking-wide">
            Metric
          </label>
          <select
            value={metric}
            onChange={(e) => setMetric(e.target.value as MetricKey)}
            className="bg-[var(--color-surface)] border border-[var(--color-border)] rounded px-2.5 py-1.5 text-sm text-[var(--color-text)]"
          >
            {METRICS.map((m) => <option key={m.key} value={m.key}>{m.label}</option>)}
          </select>
        </div>

        <div className="flex-1" />

        <span className="text-xs text-[var(--color-text-muted)] font-mono">
          {matrix.rows.length} row{matrix.rows.length !== 1 ? "s" : ""} · {orderedConditions.length} condition{orderedConditions.length !== 1 ? "s" : ""}
        </span>
      </div>

      {matrix.rows.length === 0 ? (
        <div className="text-center py-12 text-[var(--color-text-muted)]">
          No runs match the current filters.
        </div>
      ) : (
        <MatrixTable rows={matrix.rows} conditions={orderedConditions} metric={activeMetric} />
      )}
    </div>
  );
}

function MatrixTable({
  rows,
  conditions,
  metric,
}: {
  rows: MatrixRow[];
  conditions: string[];
  metric: (typeof METRICS)[number];
}) {
  // Group rows by (evalType, target, model) — a stable summary prefix —
  // so long matrices read as small blocks rather than one undifferentiated list.
  const grouped = useMemo(() => {
    const groups = new Map<string, MatrixRow[]>();
    for (const row of rows) {
      const k = `${row.key.evalType} · ${row.key.target} · ${row.key.model}`;
      const bucket = groups.get(k);
      if (bucket) bucket.push(row);
      else groups.set(k, [row]);
    }
    return groups;
  }, [rows]);

  return (
    <div className="space-y-6">
      {Array.from(grouped.entries()).map(([groupLabel, groupRows]) => (
        <div key={groupLabel} className="rounded-lg border border-[var(--color-border)] overflow-hidden">
          <div className="bg-[var(--color-surface)] border-b border-[var(--color-border)] px-3 py-2 text-xs font-mono text-[var(--color-text-muted)]">
            {groupLabel}
          </div>
          <div className="overflow-x-auto">
            <table className="w-full text-xs">
              <thead>
                <tr className="bg-[var(--color-surface)]/50">
                  <th className="px-3 py-2 text-left font-medium text-[var(--color-text-muted)] whitespace-nowrap">
                    Reasoning / Scenario / Batch
                  </th>
                  {conditions.map((cond) => (
                    <th
                      key={cond}
                      className="px-3 py-2 text-right font-medium text-[var(--color-text-muted)] whitespace-nowrap"
                    >
                      {cond}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {groupRows.map((row) => (
                  <tr key={row.id} className="border-t border-[var(--color-border)]">
                    <td className="px-3 py-1.5 whitespace-nowrap text-[var(--color-text)]">
                      <span className="font-mono text-[11px]">{row.key.reasoning}</span>
                      {row.key.scenario && (
                        <span className="ml-2 font-mono text-[11px] text-[var(--color-text-muted)]">
                          / {row.key.scenario}
                        </span>
                      )}
                      {row.key.batchId && (
                        <span className="ml-2 font-mono text-[10px] text-[var(--color-text-muted)]">
                          / {row.key.batchId.slice(0, 8)}
                        </span>
                      )}
                    </td>
                    {conditions.map((cond) => (
                      <td key={cond} className="px-3 py-1.5 text-right">
                        <CellView cell={row.cells.get(cond)} metric={metric} />
                      </td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      ))}
    </div>
  );
}
