import type { EvalResult } from "./types";

// ---------------------------------------------------------------------------
// Data shaping for the Charts page.
//
// Rows arrive flat. A Pareto scatter plots one point per (config, condition)
// batch — the same grouping the Matrix uses — so repetitions within a batch
// collapse to a mean with cross-run stdev, preserving uncertainty on the plot.
// Two points with batchId=null for the same condition/config are treated as
// separate singleton points, because we have no basis to merge them.
// ---------------------------------------------------------------------------

export interface ParetoPoint {
  /** Stable id for React keys. */
  id: string;
  condition: string;
  evalType: string;
  target: string;
  model: string;
  reasoning: string;
  scenario: string | null;
  batchId: string | null;
  /** How many repetitions contributed. 1 for singletons. */
  n: number;
  /** Mean quality score. null if every contributing run had null. */
  qualityScore: number | null;
  qualityStdev: number | null;
  /** Mean LLM-judge score. null if unavailable. */
  judgeScore: number | null;
  /** Mean cost in USD. */
  cost: number | null;
  /** Mean total tokens. */
  totalTokens: number | null;
  /** Mean duration seconds, parsed from the `duration` string. */
  durationSeconds: number | null;
  /** Worst-case deliverable status across runs. */
  deliverableStatus: EvalResult["deliverableStatus"] | null;
}

function meanOf(xs: Array<number | null | undefined>): number | null {
  const clean = xs.filter((n): n is number => typeof n === "number" && !Number.isNaN(n));
  if (clean.length === 0) return null;
  return clean.reduce((s, n) => s + n, 0) / clean.length;
}

function stdevOf(xs: Array<number | null | undefined>): number | null {
  const clean = xs.filter((n): n is number => typeof n === "number" && !Number.isNaN(n));
  if (clean.length < 2) return null;
  const m = clean.reduce((s, n) => s + n, 0) / clean.length;
  const v = clean.reduce((s, n) => s + (n - m) ** 2, 0) / (clean.length - 1);
  return Math.sqrt(v);
}

function worstDeliverable(
  runs: EvalResult[],
): EvalResult["deliverableStatus"] | null {
  const s = runs.map((r) => r.deliverableStatus ?? null);
  if (s.some((x) => x === "failed")) return "failed";
  if (s.some((x) => x === "degraded")) return "degraded";
  if (s.some((x) => x === "success")) return "success";
  return null;
}

/** "30s" → 30, "1m 45s" → 105, "-" → null. */
export function parseDuration(input: string | null | undefined): number | null {
  if (!input || input === "-") return null;
  let total = 0;
  let matched = false;
  const minMatch = input.match(/(\d+)\s*m/);
  if (minMatch) {
    total += parseInt(minMatch[1], 10) * 60;
    matched = true;
  }
  const secMatch = input.match(/(\d+)\s*s/);
  if (secMatch) {
    total += parseInt(secMatch[1], 10);
    matched = true;
  }
  return matched ? total : null;
}

export function buildParetoPoints(rows: EvalResult[]): ParetoPoint[] {
  const buckets = new Map<string, EvalResult[]>();
  // Key separates by everything the Matrix page treats as a distinct row,
  // plus condition — because that's the unit we're plotting.
  for (const r of rows) {
    const scenarioKey = r.scenario ?? "—";
    const batchKey = r.batchId ?? `single:${r.id}`;
    const key = [
      r.evalType, r.target, r.model, r.reasoning, scenarioKey, batchKey, r.condition,
    ].join("|");
    const bucket = buckets.get(key);
    if (bucket) bucket.push(r);
    else buckets.set(key, [r]);
  }

  const points: ParetoPoint[] = [];
  for (const [key, runs] of buckets) {
    const first = runs[0];
    points.push({
      id: key,
      condition: first.condition,
      evalType: first.evalType,
      target: first.target,
      model: first.model,
      reasoning: first.reasoning,
      scenario: first.scenario ?? null,
      batchId: first.batchId ?? null,
      n: runs.length,
      qualityScore: meanOf(runs.map((r) => r.qualityScore)),
      qualityStdev: stdevOf(runs.map((r) => r.qualityScore)),
      judgeScore: meanOf(runs.map((r) => r.judgeScore)),
      cost: meanOf(runs.map((r) => r.cost)),
      totalTokens: meanOf(runs.map((r) => r.totalTokens)),
      durationSeconds: meanOf(runs.map((r) => parseDuration(r.duration))),
      deliverableStatus: worstDeliverable(runs),
    });
  }
  return points;
}

/** Map a condition name to a stable color token. Used by multiple charts so
 * the viewer's intuition ("green means explore") holds across pages. */
export function conditionColor(cond: string): string {
  switch (cond) {
    case "control-cto-off":  return "#6b7280"; // slate-500, muted baseline
    case "control-cto-on":   return "#94a3b8"; // slate-400, secondary baseline
    case "control":          return "#9ca3af"; // grey-400, legacy 3-cond runs
    case "explore":          return "#10b981"; // emerald, the treatment
    case "leverage":         return "#3b82f6"; // blue, the stronger treatment
    case "task-conditioned": return "#a855f7"; // purple, task-specific variant
    default:                 return "#f97316"; // orange for unknown conditions
  }
}
