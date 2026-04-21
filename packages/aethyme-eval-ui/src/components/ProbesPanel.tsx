import type { ProbeRow } from "../lib/api";

/**
 * Summarized per-condition view of probes.
 *
 * For a batch with J conditions × K runs, we have J*K rows per probe_name.
 * This panel aggregates by condition so the user sees: for each condition,
 * how often did navigation find the target in the first K calls? How often
 * was Aethyme consulted before editing?
 */

interface NavAgg {
  runs: number;
  skipped: number;
  hit_count: number;          // rows where first_hit_step != null
  avg_first_hit: number | null;
  avg_precision_at_k: number | null;
  avg_recall_at_k: number | null;
  avg_distractor_rate: number | null;
}

interface GraphAgg {
  runs: number;
  skipped: number;
  aethyme_called_runs: number;
  aethyme_before_edit_runs: number;
  aethyme_before_edit_applicable: number; // denom excludes "no edit" cases
}

function aggregateNav(rows: ProbeRow[]): NavAgg {
  const nav = rows.filter((r) => r.probe_name === "navigation");
  const nonSkipped = nav.filter((r) => !r.result.skipped);
  const skipped = nav.length - nonSkipped.length;
  const hits = nonSkipped.filter((r) => r.result.first_hit_step != null);

  function avg(vals: number[]): number | null {
    if (vals.length === 0) return null;
    return vals.reduce((a, b) => a + b, 0) / vals.length;
  }

  return {
    runs: nav.length,
    skipped,
    hit_count: hits.length,
    avg_first_hit: avg(hits.map((r) => r.result.first_hit_step as number)),
    avg_precision_at_k: avg(
      nonSkipped.map((r) => (r.result.precision_at_k ?? 0)),
    ),
    avg_recall_at_k: avg(nonSkipped.map((r) => (r.result.recall_at_k ?? 0))),
    avg_distractor_rate: avg(
      nonSkipped.map((r) => (r.result.distractor_rate ?? 0)),
    ),
  };
}

function aggregateGraph(rows: ProbeRow[]): GraphAgg {
  const g = rows.filter((r) => r.probe_name === "graph_usage");
  const nonSkipped = g.filter((r) => !r.result.skipped);
  const skipped = g.length - nonSkipped.length;
  const called = nonSkipped.filter((r) => r.result.aethyme_called === true);
  const beforeEditRuns = nonSkipped.filter(
    (r) => r.result.aethyme_called_before_first_edit === true,
  );
  const beforeEditApplicable = nonSkipped.filter(
    (r) => r.result.aethyme_called_before_first_edit !== null,
  );
  return {
    runs: g.length,
    skipped,
    aethyme_called_runs: called.length,
    aethyme_before_edit_runs: beforeEditRuns.length,
    aethyme_before_edit_applicable: beforeEditApplicable.length,
  };
}

function fmtPct(x: number | null, digits = 0): string {
  if (x === null || x === undefined) return "—";
  return `${(x * 100).toFixed(digits)}%`;
}

function groupByCondition(rows: ProbeRow[]): Record<string, ProbeRow[]> {
  const out: Record<string, ProbeRow[]> = {};
  for (const row of rows) {
    out[row.condition] = out[row.condition] ?? [];
    out[row.condition].push(row);
  }
  return out;
}

interface Props {
  probes: ProbeRow[];
}

export default function ProbesPanel({ probes }: Props) {
  if (probes.length === 0) {
    return (
      <div className="text-xs text-[var(--color-text-muted)]">
        No probe results for this batch. Probes piggyback on Claude-JSONL
        transcripts — older runs or codex-driven runs are not probed.
      </div>
    );
  }

  const byCondition = groupByCondition(probes);
  const conditions = Object.keys(byCondition).sort();

  return (
    <div className="space-y-3 text-xs">
      <div className="flex items-center justify-between">
        <h3 className="font-semibold text-sm">Capability probes</h3>
        <span className="text-[var(--color-text-muted)]">
          per-condition aggregates across all runs in this batch
        </span>
      </div>

      <table className="w-full font-mono text-[11px]">
        <thead>
          <tr className="text-[var(--color-text-muted)] text-left border-b border-[var(--color-border)]">
            <th className="py-1 px-1">Condition</th>
            <th className="py-1 px-1" title="Runs where navigation opened a must_open file in first K calls">
              Nav hit rate
            </th>
            <th className="py-1 px-1" title="Average step at which a must_open file was first opened (lower = faster)">
              Avg first-hit
            </th>
            <th className="py-1 px-1" title="Fraction of first-K Read/Grep calls that matched must_open">
              Precision@K
            </th>
            <th className="py-1 px-1" title="Fraction of must_open files that were opened in first K calls">
              Recall@K
            </th>
            <th className="py-1 px-1" title="Fraction of first-K Read/Grep calls that matched must_not_open">
              Distractor
            </th>
            <th className="py-1 px-1" title="Runs where the agent invoked aethyme CLI or MCP tools">
              Aethyme used
            </th>
            <th className="py-1 px-1" title="Runs where aethyme was called BEFORE the first edit (informed navigation)">
              Before edit
            </th>
          </tr>
        </thead>
        <tbody>
          {conditions.map((cond) => {
            const rows = byCondition[cond];
            const nav = aggregateNav(rows);
            const graph = aggregateGraph(rows);
            return (
              <tr key={cond} className="border-b border-[var(--color-border)]">
                <td className="py-1 px-1">{cond}</td>
                <td className="py-1 px-1">
                  {nav.runs - nav.skipped === 0
                    ? "—"
                    : `${nav.hit_count}/${nav.runs - nav.skipped}`}
                </td>
                <td className="py-1 px-1">
                  {nav.avg_first_hit !== null
                    ? nav.avg_first_hit.toFixed(1)
                    : "—"}
                </td>
                <td className="py-1 px-1">
                  {fmtPct(nav.avg_precision_at_k)}
                </td>
                <td className="py-1 px-1">{fmtPct(nav.avg_recall_at_k)}</td>
                <td className="py-1 px-1">
                  {fmtPct(nav.avg_distractor_rate)}
                </td>
                <td className="py-1 px-1">
                  {graph.runs - graph.skipped === 0
                    ? "—"
                    : `${graph.aethyme_called_runs}/${
                        graph.runs - graph.skipped
                      }`}
                </td>
                <td className="py-1 px-1">
                  {graph.aethyme_before_edit_applicable === 0
                    ? "—"
                    : `${graph.aethyme_before_edit_runs}/${graph.aethyme_before_edit_applicable}`}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>

      <p className="text-[11px] text-[var(--color-text-muted)]">
        Probes observe the agent's tool-call stream. "Nav hit rate" counts
        runs where a must_open file was opened in the first K calls;
        "first-hit" is the step index. "Before edit" excludes runs with no
        edits (explain-repo style) from the denominator.
      </p>
    </div>
  );
}
