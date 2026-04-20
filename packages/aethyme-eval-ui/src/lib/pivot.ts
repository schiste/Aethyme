import type { EvalResult } from "./types";

// ---------------------------------------------------------------------------
// Matrix pivot
//
// Pivots a flat list of EvalResult rows into a matrix shape:
//   row key = (evalType, target, model, reasoning, scenario, batchId) —
//             the "what was being evaluated" tuple
//   column  = condition
//   cell    = one aggregated result (mean of runs sharing the key+condition)
//
// The pivot unit matches how evals are actually designed: apples-to-apples
// comparison across conditions for a fixed task configuration. Scenarios and
// reasoning levels are intentionally part of the row key because the user's
// research question (reasoning × navigation interaction) requires them to be
// kept distinct, not averaged together.
// ---------------------------------------------------------------------------

export interface MatrixRowKey {
  evalType: string;
  target: string;
  model: string;
  reasoning: string;
  scenario: string | null;
  batchId: string | null;
}

export interface MatrixCell {
  /** Individual runs that contributed to this cell (1+). */
  runs: EvalResult[];
  /** Mean of the qualityScore across runs. */
  qualityScore: number | null;
  /** Mean of the LLM-judge score across runs. */
  judgeScore: number | null;
  /** Mean of the recalculatedEvalScore across runs. */
  recalculatedEvalScore: number | null;
  /** Sample stdev of the quality score across runs (null when <2 runs). */
  qualityStdev: number | null;
  /** Mean totalTokens. */
  totalTokens: number | null;
  /** Mean cost (USD). */
  cost: number | null;
  /** Worst-case deliverable status across contributing runs. */
  deliverableStatus: EvalResult["deliverableStatus"] | null;
}

export interface MatrixRow {
  key: MatrixRowKey;
  /** Stable string key for React. */
  id: string;
  /** Cells keyed by condition. */
  cells: Map<string, MatrixCell>;
}

export interface Matrix {
  rows: MatrixRow[];
  /** Unique conditions seen across all rows, in the ORDER encountered — not
   * reordered, so callers can choose their own ordering preference. */
  conditions: string[];
}

function rowKeyFor(r: EvalResult): MatrixRowKey {
  return {
    evalType: r.evalType,
    target: r.target,
    model: r.model,
    reasoning: r.reasoning,
    scenario: r.scenario ?? null,
    batchId: r.batchId ?? null,
  };
}

export function rowKeyId(k: MatrixRowKey): string {
  // Pipe is not a valid identifier character for any of these fields in
  // practice; keeps IDs readable in React dev tools.
  return [
    k.evalType,
    k.target,
    k.model,
    k.reasoning,
    k.scenario ?? "—",
    k.batchId ?? "—",
  ].join("|");
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

function aggregateCell(runs: EvalResult[]): MatrixCell {
  return {
    runs,
    qualityScore: meanOf(runs.map((r) => r.qualityScore)),
    judgeScore: meanOf(runs.map((r) => r.judgeScore)),
    recalculatedEvalScore: meanOf(runs.map((r) => r.recalculatedEvalScore)),
    qualityStdev: stdevOf(runs.map((r) => r.qualityScore)),
    totalTokens: meanOf(runs.map((r) => r.totalTokens)),
    cost: meanOf(runs.map((r) => r.cost)),
    deliverableStatus: worstDeliverable(runs),
  };
}

export function pivotResults(results: EvalResult[]): Matrix {
  const rowBuckets = new Map<string, { key: MatrixRowKey; cells: Map<string, EvalResult[]> }>();
  const conditionOrder: string[] = [];
  const conditionSeen = new Set<string>();

  for (const r of results) {
    const key = rowKeyFor(r);
    const id = rowKeyId(key);
    let bucket = rowBuckets.get(id);
    if (!bucket) {
      bucket = { key, cells: new Map() };
      rowBuckets.set(id, bucket);
    }
    let cellRuns = bucket.cells.get(r.condition);
    if (!cellRuns) {
      cellRuns = [];
      bucket.cells.set(r.condition, cellRuns);
    }
    cellRuns.push(r);

    if (!conditionSeen.has(r.condition)) {
      conditionSeen.add(r.condition);
      conditionOrder.push(r.condition);
    }
  }

  const rows: MatrixRow[] = [];
  for (const [id, bucket] of rowBuckets) {
    const cells = new Map<string, MatrixCell>();
    for (const [cond, runs] of bucket.cells) {
      cells.set(cond, aggregateCell(runs));
    }
    rows.push({ id, key: bucket.key, cells });
  }

  return { rows, conditions: conditionOrder };
}

/** Canonical condition order when the user hasn't filtered — matches the
 * experimental protocol: controls first, then exploratory/leverage variants. */
export const CANONICAL_CONDITION_ORDER = [
  "control-cto-off",
  "control-cto-on",
  "control",
  "explore",
  "leverage",
  "task-conditioned",
] as const;

export function sortConditions(found: string[]): string[] {
  const known = CANONICAL_CONDITION_ORDER.filter((c) => found.includes(c));
  const unknown = found.filter(
    (c) => !(CANONICAL_CONDITION_ORDER as readonly string[]).includes(c),
  );
  unknown.sort();
  return [...known, ...unknown];
}
