import { useState, useRef, useEffect } from "react";
import { createPortal } from "react-dom";
import type { EvalResult, SortConfig, SortDirection } from "../lib/types";

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

type ColumnKey = keyof EvalResult;

const columns: { key: ColumnKey; label: string; align?: "right"; width: string }[] = [
  { key: "runId", label: "Run", width: "7%" },
  { key: "date", label: "Date", width: "8%" },
  { key: "evalType", label: "Type", width: "8%" },
  { key: "target", label: "Target", width: "6%" },
  { key: "model", label: "Model", width: "6%" },
  { key: "condition", label: "Condition", width: "12%" },
  { key: "cto", label: "CTO", width: "4%" },
  { key: "qualityScore", label: "Quality", align: "right", width: "6%" },
  { key: "recalculatedEvalScore", label: "Recalc", align: "right", width: "6%" },
  { key: "toolCalls", label: "Tools", align: "right", width: "5%" },
  { key: "totalTokens", label: "Tokens", align: "right", width: "7%" },
  { key: "duration", label: "Time", align: "right", width: "5%" },
  { key: "scorePer1kTokens", label: "Score/1K", align: "right", width: "7%" },
  { key: "scorePerMinute", label: "Score/Min", align: "right", width: "7%" },
];

function sortResults(
  results: EvalResult[],
  sort: SortConfig,
): EvalResult[] {
  return [...results].sort((a, b) => {
    const aVal = a[sort.key] ?? "";
    const bVal = b[sort.key] ?? "";
    let cmp = 0;
    if (typeof aVal === "number" && typeof bVal === "number") {
      cmp = aVal - bVal;
    } else {
      cmp = String(aVal).localeCompare(String(bVal));
    }
    return sort.direction === "asc" ? cmp : -cmp;
  });
}

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

export default function ResultsTable({ results }: Props) {
  const [sort, setSort] = useState<SortConfig>({
    key: "date",
    direction: "desc",
  });
  const [selectedResult, setSelectedResult] = useState<EvalResult | null>(null);

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

  const sorted = sortResults(results, sort);

  function renderCell(row: EvalResult, key: ColumnKey) {
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
        if (breakdown || topTools) {
          const lines = breakdown
            ? Object.entries(breakdown)
                .sort(([, a], [, b]) => b - a)
                .map(([name, count]) => `${name}: ${count}`)
            : [];
          const topLines = breakdown ? [] : (topTools || []).map((tool) => `${tool.name}: ${tool.count}`);
          const content = [...lines, ...topLines].join("\n");
          return (
            <Tooltip content={content || "No tool breakdown available"}>
              <span className="font-mono cursor-help border-b border-dotted border-[var(--color-text-muted)]">
                {row.toolCalls}
              </span>
            </Tooltip>
          );
        }
        return <span className="font-mono">{row.toolCalls}</span>;
      }
      case "scorePer1kTokens":
        return <span className="font-mono">{typeof row.scorePer1kTokens === "number" ? row.scorePer1kTokens.toFixed(4) : "—"}</span>;
      case "scorePerMinute":
        return <span className="font-mono">{typeof row.scorePerMinute === "number" ? row.scorePerMinute.toFixed(4) : "—"}</span>;
      case "evalType":
        return (
          <span
            className="cursor-pointer text-[var(--color-accent)] hover:underline"
            onClick={() => setSelectedResult(row)}
          >
            {row.evalType}
          </span>
        );
      case "target": {
        const isControl = row.condition.startsWith("control");
        const repoPaths: Record<string, [string, string]> = {
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
        if (!row.runId) return <span className="text-[var(--color-text-muted)]">—</span>;
        const short = row.runId.replace("run-", "").slice(0, 10);
        return (
          <Tooltip content={row.runId}>
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
          <span className="text-xs font-mono px-1 py-0.5 rounded bg-[var(--color-border)]/50">
            {row.condition}
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

  return (
    <>
    {selectedResult && <OutputModal result={selectedResult} onClose={() => setSelectedResult(null)} />}
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
          {sorted.map((row) => (
            <tr
              key={row.id}
              className="border-t border-[var(--color-border)] hover:bg-[var(--color-surface-hover)] transition-colors"
            >
              {columns.map((col) => (
                <td
                  key={col.key}
                  className={[
                    "px-2 py-1.5 truncate",
                    col.align === "right" ? "text-right" : "text-left",
                  ].join(" ")}
                >
                  {renderCell(row, col.key)}
                </td>
              ))}
            </tr>
          ))}
          {sorted.length === 0 && (
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
