import { useEffect, useState } from "react";
import {
  fetchBatches,
  fetchBatchAggregate,
  fetchBatchProbes,
  type BatchAggregate,
  type BatchSummary,
  type ConditionComparison,
  type ProbeRow,
} from "../lib/api";
import VarianceBreakdown from "../components/VarianceBreakdown";
import ProbesPanel from "../components/ProbesPanel";

/**
 * Batches page: list recent multi-run batches, surface each one's
 * variance breakdown when selected.
 *
 * This is the P10 read side — the write side (setting batch_id when a
 * run is launched) already exists in the /run flow. Anything with
 * runs >= 2 shows up here; runs=1 rows have no batch_id and aren't
 * listed.
 */

function verdictColor(verdict: ConditionComparison["verdict"]): string {
  if (verdict === "A>B") return "text-[var(--color-score-green)]";
  if (verdict === "B>A") return "text-[var(--color-score-red)]";
  return "text-[var(--color-text-muted)]";
}

function fmtDelta(c: ConditionComparison | undefined): string {
  if (!c || c.delta === null) return "—";
  const sign = c.delta >= 0 ? "+" : "";
  return `${sign}${c.delta}`;
}
export default function Batches() {
  const [batches, setBatches] = useState<BatchSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [aggregate, setAggregate] = useState<BatchAggregate | null>(null);
  const [aggLoading, setAggLoading] = useState(false);
  const [aggError, setAggError] = useState<string | null>(null);
  const [probes, setProbes] = useState<ProbeRow[]>([]);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const data = await fetchBatches();
        if (!cancelled) {
          setBatches(data);
          setLoading(false);
          if (data.length > 0) setSelected(data[0].batch_id);
        }
      } catch (e) {
        if (!cancelled) {
          setError(e instanceof Error ? e.message : String(e));
          setLoading(false);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (!selected) return;
    let cancelled = false;
    setAggLoading(true);
    setAggError(null);
    // Fetch aggregate and probes in parallel — they're independent reads.
    (async () => {
      try {
        const [agg, probeData] = await Promise.all([
          fetchBatchAggregate(selected),
          fetchBatchProbes(selected).catch(() => [] as ProbeRow[]),
        ]);
        if (!cancelled) {
          setAggregate(agg);
          setProbes(probeData);
          setAggLoading(false);
        }
      } catch (e) {
        if (!cancelled) {
          setAggError(e instanceof Error ? e.message : String(e));
          setAggLoading(false);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [selected]);

  if (loading) return <div className="p-4 text-sm">Loading batches…</div>;
  if (error)
    return (
      <div className="p-4 text-sm text-[var(--color-score-red)]">
        {error}
      </div>
    );
  if (batches.length === 0)
    return (
      <div className="p-4 text-sm text-[var(--color-text-muted)]">
        No multi-run batches yet. Set <code>runs ≥ 2</code> on the Run page
        to aggregate across runs; <code>runs = 1</code> rows aren't batched.
      </div>
    );

  return (
    <div className="p-4 grid grid-cols-[320px_1fr] gap-4">
      <aside>
        <h2 className="text-sm font-semibold mb-2">Recent batches</h2>
        <ul className="space-y-1">
          {batches.map((b) => {
            const isSelected = b.batch_id === selected;
            return (
              <li key={b.batch_id}>
                <button
                  onClick={() => setSelected(b.batch_id)}
                  className={`w-full text-left p-2 rounded border text-xs transition-colors ${
                    isSelected
                      ? "border-[var(--color-accent)] bg-[var(--color-surface-hover)]"
                      : "border-[var(--color-border)] hover:bg-[var(--color-surface-hover)]"
                  }`}
                >
                  <div className="font-mono text-[11px] truncate">
                    {b.batch_id}
                  </div>
                  <div className="flex items-center justify-between mt-0.5">
                    <span className="text-[var(--color-text-muted)]">
                      {b.eval_type} · {b.target} · {b.model}
                    </span>
                    <span className="font-mono">
                      {b.distinct_runs}×{b.total_rows}
                    </span>
                  </div>
                  <div className="text-[10px] text-[var(--color-text-muted)] mt-0.5">
                    {b.last_date}
                  </div>
                </button>
              </li>
            );
          })}
        </ul>
      </aside>

      <section className="space-y-4">
        {aggLoading && <div className="text-sm">Loading batch…</div>}
        {aggError && (
          <div className="text-sm text-[var(--color-score-red)]">
            {aggError}
          </div>
        )}
        {aggregate && (
          <>
            <header>
              <h2 className="text-base font-semibold">
                Batch {aggregate.batch?.batch_id as string}
              </h2>
              <div className="text-xs text-[var(--color-text-muted)]">
                {String(aggregate.batch?.eval_type)} ·{" "}
                {String(aggregate.batch?.target)} ·{" "}
                {String(aggregate.batch?.model)} ·{" "}
                pre-registered metric:{" "}
                <code>{String(aggregate.batch?.primary_metric)}</code> · min
                Δ = {String(aggregate.batch?.minimum_meaningful_delta)}
              </div>
            </header>

            <section className="p-3 border border-[var(--color-border)] rounded">
              <VarianceBreakdown variance={aggregate.variance_components} />
            </section>

            <section className="p-3 border border-[var(--color-border)] rounded">
              <ProbesPanel probes={probes} />
            </section>

            <section className="p-3 border border-[var(--color-border)] rounded text-xs">
              <h3 className="text-sm font-semibold mb-2">Conditions</h3>
              <table className="w-full font-mono text-[11px]">
                <thead>
                  <tr className="text-[var(--color-text-muted)] text-left">
                    <th className="py-1">Condition</th>
                    <th className="py-1">Quality median (IQR)</th>
                    <th className="py-1">Judge median</th>
                    <th className="py-1">Cost median</th>
                    <th className="py-1">Deliverable rate</th>
                  </tr>
                </thead>
                <tbody>
                  {Object.entries(aggregate.conditions).map(([cond, stats]) => {
                    const q = (stats as any).quality ?? {};
                    const j = (stats as any).judge ?? {};
                    const c = (stats as any).cost ?? {};
                    const d = (stats as any).deliverable_success_rate ?? {};
                    return (
                      <tr
                        key={cond}
                        className="border-t border-[var(--color-border)]"
                      >
                        <td className="py-1">{cond}</td>
                        <td className="py-1">
                          {q.median ?? "—"}
                          {q.iqr !== null && q.iqr !== undefined
                            ? ` (±${q.iqr})`
                            : ""}
                        </td>
                        <td className="py-1">{j.median ?? "—"}</td>
                        <td className="py-1">
                          {c.median ? `$${c.median.toFixed(3)}` : "—"}
                        </td>
                        <td className="py-1">
                          {d.rate !== null && d.rate !== undefined
                            ? `${(d.rate * 100).toFixed(0)}%`
                            : "—"}
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </section>

            {aggregate.stratified?.clean_only && (
              <LeakageStratificationTile
                clean={aggregate.stratified.clean_only}
                overallComparisons={aggregate.comparisons_vs_baseline}
                primaryMetric={String(
                  aggregate.batch?.primary_metric ?? "quality",
                )}
              />
            )}
          </>
        )}
      </section>
    </div>
  );
}

/**
 * Pretraining-leakage stratification tile.
 *
 * Renders the same baseline comparisons in two strata side-by-side:
 * overall (all runs) vs clean-only (runs that passed the cold probe with
 * leakage_is_clean = 1). If a tool's verdict flips from "meaningful" in the
 * overall stratum to "inconclusive" in clean-only, that's evidence the
 * effect was driven by contaminated runs. The reverse — inconclusive overall,
 * meaningful clean-only — means contamination noise was *masking* a real
 * gain.
 */
function LeakageStratificationTile({
  clean,
  overallComparisons,
  primaryMetric,
}: {
  clean: NonNullable<BatchAggregate["stratified"]>["clean_only"];
  overallComparisons: Record<string, ConditionComparison> | undefined;
  primaryMetric: string;
}) {
  const conditions = Object.keys(clean.comparisons_vs_baseline);
  const overall = overallComparisons ?? {};
  return (
    <section className="p-3 border border-[var(--color-border)] rounded text-xs">
      <h3 className="text-sm font-semibold mb-1">
        Leakage stratification{" "}
        <span className="text-[var(--color-text-muted)] font-normal">
          (clean-only vs overall)
        </span>
      </h3>
      <p className="text-[11px] text-[var(--color-text-muted)] mb-2">
        Clean stratum keeps only runs where the cold probe judged the agent
        had no prior knowledge of the scenario.{" "}
        <span className="font-mono">
          {clean.rows_total}
        </span>{" "}
        clean row{clean.rows_total === 1 ? "" : "s"} across conditions; metric:{" "}
        <code>{primaryMetric}</code>.
      </p>
      <table className="w-full font-mono text-[11px]">
        <thead>
          <tr className="text-[var(--color-text-muted)] text-left">
            <th className="py-1">Condition</th>
            <th className="py-1">n clean</th>
            <th className="py-1">Δ overall</th>
            <th className="py-1">Δ clean</th>
            <th className="py-1">Flip?</th>
          </tr>
        </thead>
        <tbody>
          {conditions.map((cond) => {
            const o = overall[cond];
            const c = clean.comparisons_vs_baseline[cond];
            const flipped =
              o && c && o.verdict !== c.verdict
                ? `${o.verdict} → ${c.verdict}`
                : "";
            return (
              <tr
                key={cond}
                className="border-t border-[var(--color-border)]"
              >
                <td className="py-1">{cond}</td>
                <td className="py-1">
                  {clean.rows_per_condition[cond] ?? 0}
                </td>
                <td className={`py-1 ${o ? verdictColor(o.verdict) : ""}`}>
                  {fmtDelta(o)}
                  {o && (
                    <span className="text-[var(--color-text-muted)] ml-1">
                      ({o.verdict})
                    </span>
                  )}
                </td>
                <td className={`py-1 ${verdictColor(c.verdict)}`}>
                  {fmtDelta(c)}
                  <span className="text-[var(--color-text-muted)] ml-1">
                    ({c.verdict})
                  </span>
                </td>
                <td
                  className={`py-1 ${
                    flipped ? "text-[var(--color-score-yellow)]" : ""
                  }`}
                >
                  {flipped || "—"}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </section>
  );
}
