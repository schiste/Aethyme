import { useState, useRef, useEffect, useMemo } from "react";
import { createPortal } from "react-dom";
import DiffModal from "./DiffModal";
import JudgeDotStrip from "./charts/JudgeDotStrip";
import ToolMixBar, { type ToolCount } from "./charts/ToolMixBar";
import type {
  DeliverableStatus,
  EvalResult,
  SortConfig,
  SortDirection,
} from "../lib/types";

interface Props {
  results: EvalResult[];
}

function OutputModal({ result, onClose }: { result: EvalResult; onClose: () => void }) {
  useEffect(() => {
    function handleKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    window.addEventListener("keydown", handleKey);
    return () => window.removeEventListener("keydown", handleKey);
  }, [onClose]);

  return createPortal(
    <div
      className="fixed inset-0 z-[9999] flex items-center justify-center bg-black/60"
      onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}
    >
      <div className="bg-[var(--color-surface)] border border-[var(--color-border)] rounded-lg shadow-2xl w-[90vw] max-w-4xl max-h-[85vh] flex flex-col">
        <div className="flex items-center justify-between px-5 py-3 border-b border-[var(--color-border)]">
          <div className="flex items-center gap-3">
            <span className="text-sm font-semibold text-[var(--color-text)]">{result.evalType}</span>
            <span className="text-xs font-mono px-1.5 py-0.5 rounded bg-[var(--color-border)]/50">{result.condition}</span>
            <span className="text-xs text-[var(--color-text-muted)]">{result.target} / {result.model}</span>
          </div>
          <button
            onClick={onClose}
            className="text-[var(--color-text-muted)] hover:text-[var(--color-text)] text-lg leading-none px-2"
          >
            ✕
          </button>
        </div>
        <div className="flex-1 overflow-y-auto px-5 py-4">
          {result.output ? (
            <pre className="text-xs font-mono text-[var(--color-text)] whitespace-pre-wrap leading-relaxed">
              {result.output}
            </pre>
          ) : (
            <p className="text-sm text-[var(--color-text-muted)] text-center py-8">No output captured for this run.</p>
          )}
        </div>
      </div>
    </div>,
    document.body,
  );
}

function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(0)}K`;
  return String(n);
}

function formatCost(n: number): string {
  return `$${n.toFixed(2)}`;
}

function formatDate(iso: string): string {
  const d = new Date(iso);
  const month = d.toLocaleDateString("en-US", { month: "short" });
  const day = d.getDate();
  const h = String(d.getHours()).padStart(2, "0");
  const m = String(d.getMinutes()).padStart(2, "0");
  return `${month} ${day} ${h}:${m}`;
}

function scoreColor(score: number): string {
  if (score >= 80) return "text-[var(--color-score-green)]";
  if (score >= 50) return "text-[var(--color-score-yellow)]";
  return "text-[var(--color-score-red)]";
}

function parseToolBreakdown(raw: string | null): Record<string, number> | null {
  if (!raw) return null;
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

function parseTopTools(raw: string | null): Array<{ name: string; count: number }> | null {
  if (!raw) return null;
  try {
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed : null;
  } catch {
    return null;
  }
}

function parseJudgeSamples(raw: string | null | undefined): number[] | null {
  if (!raw) return null;
  try {
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return null;
    // Backend stores samples in two shapes depending on when the row was
    // written: plain `[n, n, n]` or `[{score: n}, {score: n}, ...]`.
    // Accept both. Non-numeric entries are dropped.
    const out: number[] = [];
    for (const entry of parsed) {
      if (typeof entry === "number" && Number.isFinite(entry)) {
        out.push(entry);
      } else if (entry && typeof entry === "object" && typeof (entry as { score?: unknown }).score === "number") {
        out.push((entry as { score: number }).score);
      }
    }
    return out.length > 0 ? out : null;
  } catch {
    return null;
  }
}

// ---------------------------------------------------------------------------
// Batch grouping
//
// Rows sharing the same (batchId, condition) are repetitions of the same
// configuration. We collapse them into a synthetic aggregate row showing
// mean ± stdev across runs, and expose the individual runs via an expand
// chevron. Singletons pass through unchanged.
// ---------------------------------------------------------------------------

interface BatchGroup {
  kind: "batch";
  key: string;          // stable id for expand state + React key
  runs: EvalResult[];   // individual repetitions (sorted by runIndex ascending)
  aggregate: EvalResult; // mean metrics, worst-case status
}

interface SingleGroup {
  kind: "single";
  key: string;
  row: EvalResult;
}

type Group = BatchGroup | SingleGroup;

function meanOf(nums: Array<number | null | undefined>): number | null {
  const clean = nums.filter((n): n is number => typeof n === "number" && !Number.isNaN(n));
  if (clean.length === 0) return null;
  return clean.reduce((s, n) => s + n, 0) / clean.length;
}

function stdevOf(nums: Array<number | null | undefined>): number | null {
  const clean = nums.filter((n): n is number => typeof n === "number" && !Number.isNaN(n));
  if (clean.length < 2) return null;
  const m = clean.reduce((s, n) => s + n, 0) / clean.length;
  const variance = clean.reduce((s, n) => s + (n - m) ** 2, 0) / (clean.length - 1);
  return Math.sqrt(variance);
}

/** Deliverable roll-up: worst wins. failed > degraded > success > null. */
function rollupDeliverable(runs: EvalResult[]): DeliverableStatus | null {
  const statuses = runs.map((r) => r.deliverableStatus ?? null);
  if (statuses.some((s) => s === "failed")) return "failed";
  if (statuses.some((s) => s === "degraded")) return "degraded";
  if (statuses.some((s) => s === "success")) return "success";
  return null;
}

function aggregateRuns(runs: EvalResult[]): EvalResult {
  const template = runs[0];
  // Take the most recent run's date so sort-by-date feels right on batches.
  const latestDate = runs
    .map((r) => r.date)
    .sort()
    .slice(-1)[0];

  return {
    ...template,
    id: `batch:${template.batchId ?? template.id}:${template.condition}`,
    date: latestDate ?? template.date,
    runId: null,
    output: null,
    prompt: null,
    scenario: template.scenario,
    score: meanOf(runs.map((r) => r.score)) ?? 0,
    turns: Math.round(meanOf(runs.map((r) => r.turns)) ?? 0),
    toolCalls: Math.round(meanOf(runs.map((r) => r.toolCalls)) ?? 0),
    totalTokens: Math.round(meanOf(runs.map((r) => r.totalTokens)) ?? 0),
    inputTokens: Math.round(meanOf(runs.map((r) => r.inputTokens)) ?? 0),
    outputTokens: Math.round(meanOf(runs.map((r) => r.outputTokens)) ?? 0),
    cacheRead: Math.round(meanOf(runs.map((r) => r.cacheRead)) ?? 0),
    cacheCreate: Math.round(meanOf(runs.map((r) => r.cacheCreate)) ?? 0),
    cost: meanOf(runs.map((r) => r.cost)) ?? 0,
    duration: template.duration,
    qualityScore: meanOf(runs.map((r) => r.qualityScore)),
    recalculatedEvalScore: meanOf(runs.map((r) => r.recalculatedEvalScore)),
    qualityDeltaVsControl: meanOf(runs.map((r) => r.qualityDeltaVsControl)),
    tokenRatioVsControl: meanOf(runs.map((r) => r.tokenRatioVsControl)),
    timeRatioVsControl: meanOf(runs.map((r) => r.timeRatioVsControl)),
    costRatioVsControl: meanOf(runs.map((r) => r.costRatioVsControl)),
    scorePer1kTokens: meanOf(runs.map((r) => r.scorePer1kTokens)),
    scorePerMinute: meanOf(runs.map((r) => r.scorePerMinute)),
    judgeScore: meanOf(runs.map((r) => r.judgeScore)),
    judgeStdev: stdevOf(runs.map((r) => r.judgeScore)), // cross-run stdev, not intra-rater
    scorerAgreementGap: meanOf(runs.map((r) => r.scorerAgreementGap)),
    scorerAgreementDivergent: runs.some((r) => r.scorerAgreementDivergent === true) ? true : null,
    judgeModel: template.judgeModel,
    judgeReliable: runs.every((r) => r.judgeReliable === true)
      ? true
      : runs.some((r) => r.judgeReliable === false)
        ? false
        : null,
    judgeSamples: null,
    judgeError: runs.find((r) => r.judgeError)?.judgeError ?? null,
    judgeCostUsd: meanOf(runs.map((r) => r.judgeCostUsd)),
    batchId: template.batchId,
    runIndex: null,
    runsInBatch: runs.length,
    deliverableStatus: rollupDeliverable(runs),
  };
}

function groupResults(results: EvalResult[]): Group[] {
  const batchMap = new Map<string, EvalResult[]>();
  const singles: EvalResult[] = [];

  for (const r of results) {
    const isBatchable =
      r.batchId != null && (r.runsInBatch ?? 0) > 1 && r.condition != null;
    if (isBatchable) {
      const key = `${r.batchId}::${r.condition}`;
      const bucket = batchMap.get(key);
      if (bucket) bucket.push(r);
      else batchMap.set(key, [r]);
    } else {
      singles.push(r);
    }
  }

  const groups: Group[] = singles.map((row) => ({
    kind: "single",
    key: row.id,
    row,
  }));

  for (const [key, runs] of batchMap) {
    if (runs.length === 1) {
      // Only one repetition has landed so far — show it as a single row.
      groups.push({ kind: "single", key: runs[0].id, row: runs[0] });
      continue;
    }
    runs.sort((a, b) => (a.runIndex ?? 0) - (b.runIndex ?? 0));
    groups.push({ kind: "batch", key, runs, aggregate: aggregateRuns(runs) });
  }

  return groups;
}

function groupHead(group: Group): EvalResult {
  return group.kind === "batch" ? group.aggregate : group.row;
}

// ---------------------------------------------------------------------------
// Sort
// ---------------------------------------------------------------------------

type ColumnKey = keyof EvalResult;

const columns: { key: ColumnKey; label: string; align?: "right"; width: string }[] = [
  { key: "runId", label: "Run", width: "7%" },
  { key: "date", label: "Date", width: "8%" },
  { key: "evalType", label: "Type", width: "8%" },
  { key: "target", label: "Target", width: "5%" },
  { key: "model", label: "Model", width: "6%" },
  { key: "condition", label: "Condition", width: "11%" },
  { key: "cto", label: "CTO", width: "4%" },
  { key: "qualityScore", label: "Quality", align: "right", width: "5%" },
  { key: "recalculatedEvalScore", label: "Recalc", align: "right", width: "5%" },
  { key: "judgeScore", label: "Judge", align: "right", width: "6%" },
  { key: "scorerAgreementGap", label: "Δ K↔J", align: "right", width: "5%" },
  { key: "toolCalls", label: "Tools", align: "right", width: "4%" },
  { key: "totalTokens", label: "Tokens", align: "right", width: "6%" },
  { key: "duration", label: "Time", align: "right", width: "5%" },
  { key: "scorePer1kTokens", label: "Score/1K", align: "right", width: "5%" },
  { key: "scorePerMinute", label: "Score/Min", align: "right", width: "5%" },
];

function sortGroups(groups: Group[], sort: SortConfig): Group[] {
  return [...groups].sort((a, b) => {
    const aVal = groupHead(a)[sort.key] ?? "";
    const bVal = groupHead(b)[sort.key] ?? "";
    let cmp = 0;
    if (typeof aVal === "number" && typeof bVal === "number") {
      cmp = aVal - bVal;
    } else {
      cmp = String(aVal).localeCompare(String(bVal));
    }
    return sort.direction === "asc" ? cmp : -cmp;
  });
}

// ---------------------------------------------------------------------------
// Tooltip
// ---------------------------------------------------------------------------

function Tooltip({ children, content }: { children: React.ReactNode; content: React.ReactNode }) {
  const [pos, setPos] = useState<{ x: number; y: number } | null>(null);
  const ref = useRef<HTMLSpanElement>(null);

  function handleEnter() {
    if (ref.current) {
      const rect = ref.current.getBoundingClientRect();
      setPos({ x: rect.left + rect.width / 2, y: rect.top });
    }
  }

  return (
    <span
      ref={ref}
      onMouseEnter={handleEnter}
      onMouseLeave={() => setPos(null)}
    >
      {children}
      {pos && createPortal(
        <div
          className="fixed z-[9999] px-3 py-2 rounded bg-[#1a1a2e] border border-[var(--color-border)] shadow-xl text-xs font-mono whitespace-pre text-[var(--color-text)] pointer-events-none"
          style={{ left: pos.x, top: pos.y - 8, transform: "translate(-50%, -100%)" }}
        >
          {content}
        </div>,
        document.body,
      )}
    </span>
  );
}

// ---------------------------------------------------------------------------
// Deliverable badge
// ---------------------------------------------------------------------------

function DeliverableDot({ status }: { status: DeliverableStatus | null | undefined }) {
  const cfg: Record<
    "success" | "degraded" | "failed" | "unknown",
    { color: string; label: string }
  > = {
    success: { color: "bg-[var(--color-score-green)]", label: "Deliverable: success" },
    degraded: { color: "bg-[var(--color-score-yellow)]", label: "Deliverable: degraded (present but malformed)" },
    failed: { color: "bg-[var(--color-score-red)]", label: "Deliverable: failed (not produced)" },
    unknown: { color: "bg-[var(--color-border)]", label: "Deliverable status: unknown (legacy row)" },
  };
  const entry = cfg[status ?? "unknown"];
  return (
    <Tooltip content={entry.label}>
      <span
        className={`inline-block w-2 h-2 rounded-full ${entry.color} cursor-help`}
        aria-label={entry.label}
      />
    </Tooltip>
  );
}

// ---------------------------------------------------------------------------
// Main component
// ---------------------------------------------------------------------------

export default function ResultsTable({ results }: Props) {
  const [sort, setSort] = useState<SortConfig>({
    key: "date",
    direction: "desc",
  });
  const [selectedResult, setSelectedResult] = useState<EvalResult | null>(null);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  // Diff selection — rows chosen for side-by-side comparison. Cap at 2:
  // any third click kicks out the oldest. Batch-aggregate rows are
  // deliberately not selectable since they have no concrete `output`.
  const [diffSelection, setDiffSelection] = useState<EvalResult[]>([]);
  const [showDiff, setShowDiff] = useState(false);

  function toggleDiffSelection(row: EvalResult) {
    setDiffSelection((prev) => {
      const exists = prev.find((r) => r.id === row.id);
      if (exists) return prev.filter((r) => r.id !== row.id);
      if (prev.length >= 2) return [prev[1], row];
      return [...prev, row];
    });
  }

  const groups = useMemo(() => groupResults(results), [results]);
  const sortedGroups = useMemo(() => sortGroups(groups, sort), [groups, sort]);

  function handleSort(key: ColumnKey) {
    setSort((prev) => {
      if (prev.key === key) {
        const direction: SortDirection =
          prev.direction === "asc" ? "desc" : "asc";
        return { key, direction };
      }
      return { key, direction: "desc" };
    });
  }

  function toggleExpand(key: string) {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }

  function renderCell(
    row: EvalResult,
    key: ColumnKey,
    ctx: { isBatch: boolean; isChild: boolean; runCount?: number },
  ) {
    switch (key) {
      case "date":
        return <span className="text-xs font-mono whitespace-nowrap">{formatDate(row.date)}</span>;
      case "score":
        return (
          <span className={`font-mono font-semibold ${scoreColor(row.score)}`}>
            {row.score}
          </span>
        );
      case "qualityScore": {
        const value = row.qualityScore ?? row.score;
        return (
          <span className={`font-mono font-semibold ${scoreColor(value ?? 0)}`}>
            {typeof value === "number" ? value.toFixed(2) : "—"}
          </span>
        );
      }
      case "recalculatedEvalScore": {
        const value = row.recalculatedEvalScore;
        const delta = row.qualityDeltaVsControl;
        const tokenRatio = row.tokenRatioVsControl;
        const timeRatio = row.timeRatioVsControl;
        const costRatio = row.costRatioVsControl;
        const lines = [
          `Quality Δ vs control: ${typeof delta === "number" ? delta.toFixed(2) : "—"}`,
          `Token ratio vs control: ${typeof tokenRatio === "number" ? tokenRatio.toFixed(4) : "—"}`,
          `Time ratio vs control:  ${typeof timeRatio === "number" ? timeRatio.toFixed(4) : "—"}`,
          `Cost ratio vs control:  ${typeof costRatio === "number" ? costRatio.toFixed(4) : "—"}`,
        ].join("\n");
        return (
          <Tooltip content={lines}>
            <span className={`font-mono font-semibold cursor-help border-b border-dotted border-[var(--color-text-muted)] ${scoreColor(value ?? 0)}`}>
              {typeof value === "number" ? value.toFixed(2) : "—"}
            </span>
          </Tooltip>
        );
      }
      case "judgeScore": {
        const value = row.judgeScore;
        if (typeof value !== "number") {
          if (row.judgeError) {
            return (
              <Tooltip content={`Judge error:\n${row.judgeError}`}>
                <span className="font-mono text-[var(--color-score-red)] cursor-help border-b border-dotted border-[var(--color-score-red)]/50">
                  err
                </span>
              </Tooltip>
            );
          }
          return <span className="text-[var(--color-text-muted)]">—</span>;
        }
        const stdev = row.judgeStdev;
        const samples = parseJudgeSamples(row.judgeSamples);
        const reliability =
          row.judgeReliable === true
            ? "reliable"
            : row.judgeReliable === false
              ? "unreliable"
              : "n/a";
        const display = typeof stdev === "number"
          ? `${value.toFixed(2)} ± ${stdev.toFixed(2)}`
          : value.toFixed(2);
        const tooltipLines = [
          `Judge score: ${value.toFixed(2)}`,
          typeof stdev === "number" ? `Stdev: ${stdev.toFixed(2)}` : null,
          `Model: ${row.judgeModel ?? "—"}`,
          `Reliability: ${reliability}`,
          samples ? `Samples: [${samples.map((s) => s.toFixed(2)).join(", ")}]` : null,
          row.judgeError ? `Error: ${row.judgeError}` : null,
        ].filter(Boolean).join("\n");
        const unreliable = row.judgeReliable === false;
        const colorClass = unreliable
          ? "text-[var(--color-score-yellow)]"
          : scoreColor(value);
        // Inline dot strip shows the N samples' spread relative to the mean —
        // tight cluster = reliable, fan-out = ambiguous answer.
        // Only render when we have 2+ samples; a single sample is
        // uninformative about spread.
        const showStrip = samples !== null && samples.length >= 2;
        return (
          <Tooltip content={tooltipLines}>
            <span
              className={`inline-flex flex-col items-end gap-0.5 cursor-help ${colorClass}`}
            >
              <span
                className={`font-mono font-semibold border-b border-dotted ${
                  unreliable
                    ? "border-[var(--color-score-yellow)]/50"
                    : "border-[var(--color-text-muted)]"
                }`}
              >
                {display}
              </span>
              {showStrip && (
                <JudgeDotStrip samples={samples} mean={value} width={60} height={10} />
              )}
            </span>
          </Tooltip>
        );
      }
      case "scorerAgreementGap": {
        const gap = row.scorerAgreementGap;
        const divergent = row.scorerAgreementDivergent === true;
        if (typeof gap !== "number") {
          return <span className="text-[var(--color-text-muted)]">—</span>;
        }
        const tooltipLines = [
          `|Judge − Quality| = ${gap.toFixed(2)}`,
          divergent
            ? "Flagged: gap exceeds divergence threshold (default 10)."
            : "Within agreement threshold.",
          "Diagnostic only — no automated downstream action.",
        ].join("\n");
        return (
          <Tooltip content={tooltipLines}>
            <span
              className={`font-mono cursor-help border-b border-dotted ${
                divergent
                  ? "text-[var(--color-score-yellow)] border-[var(--color-score-yellow)]/50"
                  : "text-[var(--color-text-muted)] border-[var(--color-text-muted)]"
              }`}
            >
              {gap.toFixed(1)}
              {divergent && <span className="ml-1">⚠</span>}
            </span>
          </Tooltip>
        );
      }
      case "totalTokens": {
        const tokenLines = [
          `Input:        ${formatTokens(row.inputTokens || 0)}`,
          `Output:       ${formatTokens(row.outputTokens || 0)}`,
          `Cache read:   ${formatTokens(row.cacheRead || 0)}`,
          `Cache create: ${formatTokens(row.cacheCreate || 0)}`,
        ].join("\n");
        return (
          <Tooltip content={tokenLines}>
            <span className="font-mono cursor-help border-b border-dotted border-[var(--color-text-muted)]">
              {formatTokens(row.totalTokens)}
            </span>
          </Tooltip>
        );
      }
      case "cost": {
        const totalInput = (row.inputTokens || 0) + (row.cacheRead || 0) + (row.cacheCreate || 0);
        const costLines = [
          `Input tokens:  ${formatTokens(totalInput)} × rate`,
          `Output tokens: ${formatTokens(row.outputTokens || 0)} × rate`,
          `= $${row.cost.toFixed(4)}`,
        ].join("\n");
        return (
          <Tooltip content={costLines}>
            <span className="font-mono cursor-help border-b border-dotted border-[var(--color-text-muted)]">
              {formatCost(row.cost)}
            </span>
          </Tooltip>
        );
      }
      case "turns":
        return <span className="font-mono">{row[key]}</span>;
      case "toolCalls": {
        const breakdown = parseToolBreakdown(row.toolBreakdown);
        const topTools = parseTopTools(row.topTools ?? null);
        // Normalize to a ToolCount[] for the stacked bar, regardless of
        // which shape the backend gave us.
        const toolCounts: ToolCount[] = breakdown
          ? Object.entries(breakdown).map(([name, count]) => ({ name, count }))
          : (topTools ?? []);
        if (toolCounts.length === 0) {
          return <span className="font-mono">{row.toolCalls}</span>;
        }
        const content = toolCounts
          .slice()
          .sort((a, b) => b.count - a.count)
          .map((t) => `${t.name}: ${t.count}`)
          .join("\n");
        return (
          <Tooltip content={content}>
            <span className="inline-flex flex-col items-end gap-0.5 cursor-help">
              <span className="font-mono border-b border-dotted border-[var(--color-text-muted)]">
                {row.toolCalls}
              </span>
              <ToolMixBar tools={toolCounts} width={60} height={6} />
            </span>
          </Tooltip>
        );
      }
      case "scorePer1kTokens":
        return <span className="font-mono">{typeof row.scorePer1kTokens === "number" ? row.scorePer1kTokens.toFixed(4) : "—"}</span>;
      case "scorePerMinute":
        return <span className="font-mono">{typeof row.scorePerMinute === "number" ? row.scorePerMinute.toFixed(4) : "—"}</span>;
      case "evalType": {
        if (ctx.isBatch) {
          return (
            <span className="inline-flex items-center gap-1 text-[var(--color-text)]">
              {row.evalType}
              <span className="text-[10px] font-mono text-[var(--color-text-muted)] px-1 rounded bg-[var(--color-border)]/40">
                ×{ctx.runCount}
              </span>
            </span>
          );
        }
        return (
          <span
            className="cursor-pointer text-[var(--color-accent)] hover:underline"
            onClick={() => setSelectedResult(row)}
          >
            {row.evalType}
          </span>
        );
      }
      case "target": {
        const isControl = row.condition.startsWith("control");
        const repoPaths: Record<string, [string, string]> = {
          "aethyme": ["~/Playground/Aethyme/Aethyme - Control", "~/Playground/Aethyme/Aethyme - Aethyme"],
          "grc": ["~/Playground/GRC/Playground Control", "~/Playground/GRC/Playground Aethyme"],
          "mediawiki": ["~/Playground/Mediawiki/Mediawiki - Control", "~/Playground/Mediawiki/Mediawiki - Aethyme"],
        };
        const pair = repoPaths[row.target];
        const actualPath = pair ? (isControl ? pair[0] : pair[1]) : row.target;
        const repoType = isControl ? "Control (vanilla)" : "Aethyme (skill + index)";
        return (
          <Tooltip content={`${repoType}\n${actualPath}`}>
            <span className="cursor-help border-b border-dotted border-[var(--color-text-muted)]">
              {row.target}
            </span>
          </Tooltip>
        );
      }
      case "model": {
        const modelInfo: Record<string, string> = {
          "haiku": "Claude Haiku 4.5\nAnthropic\nInput: $0.80/M  Output: $4.00/M",
          "sonnet": "Claude Sonnet 4\nAnthropic\nInput: $3.00/M  Output: $15.00/M",
          "opus": "Claude Opus 4\nAnthropic\nInput: $15.00/M  Output: $75.00/M",
          "gpt-5.4": "GPT-5.4\nOpenAI\nInput: $2.00/M  Output: $8.00/M",
        };
        const info = modelInfo[row.model] || row.model;
        const details = `${info}\nReasoning: ${row.reasoning}`;
        return (
          <Tooltip content={details}>
            <span className="cursor-help border-b border-dotted border-[var(--color-text-muted)]">
              {row.model}
            </span>
          </Tooltip>
        );
      }
      case "runId": {
        if (ctx.isBatch) {
          return (
            <span className="font-mono text-[10px] text-[var(--color-text-muted)]">
              batch ×{ctx.runCount}
            </span>
          );
        }
        if (!row.runId) return <span className="text-[var(--color-text-muted)]">—</span>;
        const short = row.runId.replace("run-", "").slice(0, 10);
        const indexSuffix =
          typeof row.runIndex === "number" && typeof row.runsInBatch === "number" && row.runsInBatch > 1
            ? ` (${row.runIndex}/${row.runsInBatch})`
            : "";
        return (
          <Tooltip content={`${row.runId}${indexSuffix}`}>
            <span className="font-mono text-[var(--color-text-muted)] cursor-help border-b border-dotted border-[var(--color-border)]">
              {short}
            </span>
          </Tooltip>
        );
      }
      case "duration":
        return <span className="font-mono">{row.duration}</span>;
      case "condition":
        return (
          <span className="inline-flex items-center gap-1.5">
            <DeliverableDot status={row.deliverableStatus ?? null} />
            <span className="text-xs font-mono px-1 py-0.5 rounded bg-[var(--color-border)]/50">
              {row.condition}
            </span>
          </span>
        );
      case "cto":
        return (
          <span className={`text-xs font-mono px-1 py-0.5 rounded ${
            row.cto === "on" ? "bg-[var(--color-score-green)]/15 text-[var(--color-score-green)]" :
            row.cto === "off" ? "bg-[var(--color-score-red)]/15 text-[var(--color-score-red)]" :
            "bg-[var(--color-border)]/50 text-[var(--color-text-muted)]"
          }`}>
            {row.cto}
          </span>
        );
      default:
        return <span className="text-xs">{String(row[key] ?? "")}</span>;
    }
  }

  function renderRow(
    row: EvalResult,
    opts: {
      rowKey: string;
      isBatch: boolean;
      isChild: boolean;
      isExpanded?: boolean;
      runCount?: number;
      onToggle?: () => void;
    },
  ) {
    const { rowKey, isBatch, isChild, isExpanded, runCount, onToggle } = opts;
    return (
      <tr
        key={rowKey}
        className={[
          "border-t border-[var(--color-border)] transition-colors",
          isChild
            ? "bg-[var(--color-border)]/10 hover:bg-[var(--color-border)]/20"
            : "hover:bg-[var(--color-surface-hover)]",
          isBatch ? "font-medium" : "",
        ].join(" ")}
      >
        {columns.map((col, idx) => (
          <td
            key={col.key}
            className={[
              "px-2 py-1.5 truncate",
              col.align === "right" ? "text-right" : "text-left",
            ].join(" ")}
          >
            {idx === 0 ? (
              <span className="inline-flex items-center gap-1">
                {isBatch ? (
                  <button
                    onClick={onToggle}
                    className="w-4 text-center text-[var(--color-text-muted)] hover:text-[var(--color-text)] font-mono text-[10px] leading-none"
                    aria-label={isExpanded ? "Collapse batch" : "Expand batch"}
                  >
                    {isExpanded ? "▾" : "▸"}
                  </button>
                ) : (
                  <input
                    type="checkbox"
                    checked={diffSelection.some((r) => r.id === row.id)}
                    onChange={() => toggleDiffSelection(row)}
                    className="w-3 h-3 accent-[var(--color-accent)] cursor-pointer"
                    aria-label="Select for diff"
                    title="Select for side-by-side diff (max 2)"
                  />
                )}
                {isChild && (
                  <span className="w-3 text-center text-[var(--color-text-muted)] font-mono text-[10px]">
                    └
                  </span>
                )}
                {renderCell(row, col.key, { isBatch, isChild, runCount })}
              </span>
            ) : (
              renderCell(row, col.key, { isBatch, isChild, runCount })
            )}
          </td>
        ))}
      </tr>
    );
  }

  const rendered: React.ReactNode[] = [];
  for (const group of sortedGroups) {
    if (group.kind === "single") {
      rendered.push(
        renderRow(group.row, {
          rowKey: group.key,
          isBatch: false,
          isChild: false,
        }),
      );
    } else {
      const isExpanded = expanded.has(group.key);
      rendered.push(
        renderRow(group.aggregate, {
          rowKey: group.key,
          isBatch: true,
          isChild: false,
          isExpanded,
          runCount: group.runs.length,
          onToggle: () => toggleExpand(group.key),
        }),
      );
      if (isExpanded) {
        for (const run of group.runs) {
          rendered.push(
            renderRow(run, {
              rowKey: `${group.key}:${run.id}`,
              isBatch: false,
              isChild: true,
            }),
          );
        }
      }
    }
  }

  return (
    <>
    {selectedResult && <OutputModal result={selectedResult} onClose={() => setSelectedResult(null)} />}
    {showDiff && diffSelection.length === 2 && (
      <DiffModal
        a={diffSelection[0]}
        b={diffSelection[1]}
        onClose={() => setShowDiff(false)}
      />
    )}
    {diffSelection.length > 0 && (
      <div className="flex items-center justify-between px-3 py-2 mb-2 rounded border border-[var(--color-accent)]/40 bg-[var(--color-accent)]/8 text-xs">
        <div className="flex items-center gap-3 text-[var(--color-text)]">
          <span className="font-mono">{diffSelection.length}/2 selected</span>
          {diffSelection.map((r) => (
            <span key={r.id} className="font-mono px-1.5 py-0.5 rounded bg-[var(--color-border)]/40">
              {r.condition}
              <button
                onClick={() => toggleDiffSelection(r)}
                className="ml-1.5 text-[var(--color-text-muted)] hover:text-[var(--color-text)]"
                aria-label={`Remove ${r.condition} from diff selection`}
              >
                ✕
              </button>
            </span>
          ))}
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={() => setDiffSelection([])}
            className="px-2 py-1 rounded text-[var(--color-text-muted)] hover:text-[var(--color-text)]"
          >
            Clear
          </button>
          <button
            onClick={() => setShowDiff(true)}
            disabled={diffSelection.length !== 2}
            className="px-3 py-1 rounded border border-[var(--color-accent)] text-[var(--color-accent)] hover:bg-[var(--color-accent)]/15 disabled:opacity-40 disabled:cursor-not-allowed"
          >
            Compare outputs →
          </button>
        </div>
      </div>
    )}
    <div className="rounded-lg border border-[var(--color-border)]">
      <table className="w-full text-xs table-fixed">
        <thead>
          <tr className="bg-[var(--color-surface)]">
            {columns.map((col) => (
              <th
                key={col.key}
                onClick={() => handleSort(col.key)}
                style={{ width: col.width }}
                className={[
                  "px-2 py-2 font-medium text-[var(--color-text-muted)] whitespace-nowrap cursor-pointer select-none hover:text-[var(--color-text)] transition-colors",
                  col.align === "right" ? "text-right" : "text-left",
                ].join(" ")}
              >
                <span className="inline-flex items-center gap-1">
                  {col.label}
                  {sort.key === col.key && (
                    <span className="text-[var(--color-accent)]">
                      {sort.direction === "asc" ? "\u2191" : "\u2193"}
                    </span>
                  )}
                </span>
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rendered}
          {rendered.length === 0 && (
            <tr>
              <td
                colSpan={columns.length}
                className="px-3 py-8 text-center text-[var(--color-text-muted)]"
              >
                No results match the current filters.
              </td>
            </tr>
          )}
        </tbody>
      </table>
    </div>
    </>
  );
}
