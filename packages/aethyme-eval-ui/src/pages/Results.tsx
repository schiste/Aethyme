import { useState, useMemo, useEffect } from "react";
import ResultsTable from "../components/ResultsTable";
import { fetchResults } from "../lib/api";
import type { EvalResult, EvalType, TargetName, ModelName } from "../lib/types";

const ALL = "__all__";

function toCSV(results: EvalResult[]): string {
  const headers = [
    "date",
    "evalType",
    "target",
    "model",
    "condition",
    "reasoning",
    "score",
    "turns",
    "toolCalls",
    "totalTokens",
    "cost",
    "duration",
    "fixed",
  ];
  const rows = results.map((r: EvalResult) =>
    headers.map((h) => String(r[h as keyof EvalResult])).join(","),
  );
  return [headers.join(","), ...rows].join("\n");
}

function download(content: string, filename: string, mime: string) {
  const blob = new Blob([content], { type: mime });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  a.click();
  URL.revokeObjectURL(url);
}

export default function Results() {
  const [results, setResults] = useState<EvalResult[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [evalTypeFilter, setEvalTypeFilter] = useState<string>(ALL);
  const [targetFilter, setTargetFilter] = useState<string>(ALL);
  const [modelFilter, setModelFilter] = useState<string>(ALL);

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
    return results.filter((r: EvalResult) => {
      if (evalTypeFilter !== ALL && r.evalType !== evalTypeFilter) return false;
      if (targetFilter !== ALL && r.target !== targetFilter) return false;
      if (modelFilter !== ALL && r.model !== modelFilter) return false;
      return true;
    });
  }, [results, evalTypeFilter, targetFilter, modelFilter]);

  const evalTypes: EvalType[] = ["bug-fix", "bug-fix-1", "explain-repo", "navigation-ctf", "cross-package"];
  const targets: TargetName[] = ["grc", "mediawiki"];
  const models: ModelName[] = ["haiku", "sonnet", "opus", "gpt-5.4"];

  function handleExportJSON() {
    download(
      JSON.stringify(filtered, null, 2),
      "eval-results.json",
      "application/json",
    );
  }

  function handleExportCSV() {
    download(toCSV(filtered), "eval-results.csv", "text/csv");
  }

  return (
    <div className="space-y-4">
      {/* Toolbar */}
      <div className="flex flex-wrap items-center gap-3">
        <div className="flex items-center gap-2">
          <label className="text-xs text-[var(--color-text-muted)] uppercase tracking-wide">
            Eval Type
          </label>
          <select
            value={evalTypeFilter}
            onChange={(e) => setEvalTypeFilter(e.target.value)}
            className="bg-[var(--color-surface)] border border-[var(--color-border)] rounded px-2.5 py-1.5 text-sm text-[var(--color-text)] focus:outline-none focus:border-[var(--color-accent)]"
          >
            <option value={ALL}>All</option>
            {evalTypes.map((t) => (
              <option key={t} value={t}>
                {t}
              </option>
            ))}
          </select>
        </div>

        <div className="flex items-center gap-2">
          <label className="text-xs text-[var(--color-text-muted)] uppercase tracking-wide">
            Target
          </label>
          <select
            value={targetFilter}
            onChange={(e) => setTargetFilter(e.target.value)}
            className="bg-[var(--color-surface)] border border-[var(--color-border)] rounded px-2.5 py-1.5 text-sm text-[var(--color-text)] focus:outline-none focus:border-[var(--color-accent)]"
          >
            <option value={ALL}>All</option>
            {targets.map((t) => (
              <option key={t} value={t}>
                {t}
              </option>
            ))}
          </select>
        </div>

        <div className="flex items-center gap-2">
          <label className="text-xs text-[var(--color-text-muted)] uppercase tracking-wide">
            Model
          </label>
          <select
            value={modelFilter}
            onChange={(e) => setModelFilter(e.target.value)}
            className="bg-[var(--color-surface)] border border-[var(--color-border)] rounded px-2.5 py-1.5 text-sm text-[var(--color-text)] focus:outline-none focus:border-[var(--color-accent)]"
          >
            <option value={ALL}>All</option>
            {models.map((m) => (
              <option key={m} value={m}>
                {m}
              </option>
            ))}
          </select>
        </div>

        <div className="flex-1" />

        <span className="text-xs text-[var(--color-text-muted)] font-mono">
          {filtered.length} result{filtered.length !== 1 ? "s" : ""}
        </span>

        <div className="flex gap-2">
          <button
            onClick={handleExportJSON}
            className="px-3 py-1.5 text-xs font-medium rounded border border-[var(--color-border)] text-[var(--color-text-muted)] hover:text-[var(--color-text)] hover:border-[var(--color-accent)] transition-colors"
          >
            Export JSON
          </button>
          <button
            onClick={handleExportCSV}
            className="px-3 py-1.5 text-xs font-medium rounded border border-[var(--color-border)] text-[var(--color-text-muted)] hover:text-[var(--color-text)] hover:border-[var(--color-accent)] transition-colors"
          >
            Export CSV
          </button>
        </div>
      </div>

      {/* Table */}
      {loading ? (
        <div className="text-center py-12 text-[var(--color-text-muted)]">Loading results...</div>
      ) : error ? (
        <div className="text-center py-12 text-[var(--color-score-red)]">
          <p className="text-lg mb-2">Failed to load results</p>
          <p className="text-sm font-mono">{error}</p>
          <p className="text-sm mt-2 text-[var(--color-text-muted)]">Make sure the backend server is running on port 8420.</p>
        </div>
      ) : results.length === 0 ? (
        <div className="text-center py-12 text-[var(--color-text-muted)]">
          <p className="text-lg mb-2">No eval runs yet</p>
          <p className="text-sm">Run an evaluation from the Run Evals tab to see results here.</p>
        </div>
      ) : (
        <ResultsTable results={filtered} />
      )}
    </div>
  );
}
