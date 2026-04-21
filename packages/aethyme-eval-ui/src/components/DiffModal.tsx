import { useEffect, useMemo } from "react";
import { createPortal } from "react-dom";
import type { EvalResult } from "../lib/types";
import { diffLines, type DiffRow } from "../lib/diff";

interface Props {
  a: EvalResult;
  b: EvalResult;
  onClose: () => void;
}

function Header({ a, b }: { a: EvalResult; b: EvalResult }) {
  return (
    <div className="flex items-center justify-between px-5 py-3 border-b border-[var(--color-border)] shrink-0">
      <div className="flex items-center gap-3">
        <span className="text-sm font-semibold text-[var(--color-text)]">
          {a.evalType} · {a.target} · {a.model}
        </span>
      </div>
      <div className="text-xs text-[var(--color-text-muted)] font-mono">
        A: <span className="text-[var(--color-text)]">{a.condition}</span>
        <span className="mx-2">vs</span>
        B: <span className="text-[var(--color-text)]">{b.condition}</span>
      </div>
    </div>
  );
}

function StatsBar({
  matched, onlyA, onlyB, similarityPct,
}: { matched: number; onlyA: number; onlyB: number; similarityPct: number }) {
  return (
    <div className="px-5 py-2 border-b border-[var(--color-border)] text-xs text-[var(--color-text-muted)] flex gap-4 shrink-0">
      <span>
        Similarity: <span className="font-mono text-[var(--color-text)]">{similarityPct}%</span>
      </span>
      <span>
        Matched: <span className="font-mono text-[var(--color-score-green)]">{matched}</span>
      </span>
      <span>
        Only A: <span className="font-mono text-[var(--color-score-red)]">{onlyA}</span>
      </span>
      <span>
        Only B: <span className="font-mono text-[var(--color-score-green)]">{onlyB}</span>
      </span>
    </div>
  );
}

function rowBg(op: DiffRow["op"], side: "a" | "b"): string {
  if (op === "same") return "bg-transparent";
  if (op === "onlyA") return side === "a" ? "bg-[var(--color-score-red)]/15" : "bg-[var(--color-border)]/20";
  // onlyB
  return side === "b" ? "bg-[var(--color-score-green)]/15" : "bg-[var(--color-border)]/20";
}

function LineRow({ row }: { row: DiffRow }) {
  return (
    <div className="grid grid-cols-[3rem_1fr_3rem_1fr] text-[11px] font-mono leading-5 hover:bg-[var(--color-surface-hover)]/30">
      <div className={`text-right pr-2 text-[var(--color-text-muted)] select-none ${rowBg(row.op, "a")}`}>
        {row.aIndex !== null ? row.aIndex + 1 : ""}
      </div>
      <div className={`px-2 whitespace-pre-wrap break-words text-[var(--color-text)] ${rowBg(row.op, "a")}`}>
        {row.aText ?? " "}
      </div>
      <div className={`text-right pr-2 text-[var(--color-text-muted)] select-none ${rowBg(row.op, "b")}`}>
        {row.bIndex !== null ? row.bIndex + 1 : ""}
      </div>
      <div className={`px-2 whitespace-pre-wrap break-words text-[var(--color-text)] ${rowBg(row.op, "b")}`}>
        {row.bText ?? " "}
      </div>
    </div>
  );
}

export default function DiffModal({ a, b, onClose }: Props) {
  useEffect(() => {
    function handleKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    window.addEventListener("keydown", handleKey);
    return () => window.removeEventListener("keydown", handleKey);
  }, [onClose]);

  const diff = useMemo(
    () => diffLines(a.output ?? "", b.output ?? ""),
    [a.output, b.output],
  );

  return createPortal(
    <div
      className="fixed inset-0 z-[9999] flex items-center justify-center bg-black/60"
      onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}
    >
      <div className="bg-[var(--color-surface)] border border-[var(--color-border)] rounded-lg shadow-2xl w-[95vw] max-w-6xl max-h-[90vh] flex flex-col">
        <Header a={a} b={b} />
        <StatsBar {...diff.stats} />
        <div className="flex-1 overflow-auto">
          {(a.output == null && b.output == null) ? (
            <div className="text-center text-sm text-[var(--color-text-muted)] py-12">
              Neither run captured output.
            </div>
          ) : diff.rows.length === 0 ? (
            <div className="text-center text-sm text-[var(--color-text-muted)] py-12">
              Both outputs empty.
            </div>
          ) : (
            <div>
              <div className="grid grid-cols-[3rem_1fr_3rem_1fr] text-[10px] font-mono text-[var(--color-text-muted)] uppercase sticky top-0 bg-[var(--color-surface)] border-b border-[var(--color-border)]">
                <div className="text-right pr-2 py-1 select-none">#</div>
                <div className="px-2 py-1">A · {a.condition}</div>
                <div className="text-right pr-2 py-1 select-none">#</div>
                <div className="px-2 py-1">B · {b.condition}</div>
              </div>
              {diff.rows.map((row, idx) => (
                <LineRow key={idx} row={row} />
              ))}
            </div>
          )}
        </div>
        <div className="px-5 py-2 border-t border-[var(--color-border)] text-xs text-[var(--color-text-muted)] flex items-center justify-between shrink-0">
          <span>Press <kbd className="px-1 py-0.5 rounded bg-[var(--color-border)]/40 text-[10px] font-mono">Esc</kbd> to close</span>
          <button
            onClick={onClose}
            className="px-3 py-1 text-xs rounded border border-[var(--color-border)] hover:border-[var(--color-accent)] text-[var(--color-text)]"
          >
            Close
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
