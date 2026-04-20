import { useState, useMemo, useEffect } from "react";
import ResultsTable from "../components/ResultsTable";
import { fetchResults } from "../lib/api";
import type { EvalResult, EvalType, TargetName, ModelName } from "../lib/types";

const ALL = "__all__";

function toCSV(results: EvalResult[]): string {
  const headers = [
    "date",
    "runId",
    "evalType",
    "target",
    "model",
    "condition",
    "reasoning",
    "score",
    "qualityScore",
    "recalculatedEvalScore",
    "qualityDeltaVsControl",
    "judgeScore",
    "judgeStdev",
    "judgeReliable",
    "judgeModel",
    "deliverableStatus",
    "turns",
    "toolCalls",
    "totalTokens",
    "cost",
    "duration",
    "scorePer1kTokens",
    "scorePerMinute",
    "fixed",
    "batchId",
    "runIndex",
    "runsInBatch",
  ];
  const rows = results.map((r: EvalResult) =>
    headers
      .map((h) => {
        const v = r[h as keyof EvalResult];
        if (v == null) return "";
        const s = String(v);
        // Minimal CSV escaping — wrap any field containing comma/quote/newline in quotes.
        return /[",\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
      })
      .join(","),
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
  const [divergentOnly, setDivergentOnly] = useState(false);

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

  const divergentCount = useMemo(
    () => results.filter((r) => r.scorerAgreementDivergent === true).length,
    [results],
  );

  const filtered = useMemo(() => {
    return results.filter((r: EvalResult) => {
      if (evalTypeFilter !== ALL && r.evalType !== evalTypeFilter) return false;
      if (targetFilter !== ALL && r.target !== targetFilter) return false;
      if (modelFilter !== ALL && r.model !== modelFilter) return false;
      if (divergentOnly && r.scorerAgreementDivergent !== true) return false;
      return true;
    });
  }, [results, evalTypeFilter, targetFilter, modelFilter, divergentOnly]);

  const evalTypes: EvalType[] = ["bug-fix", "bug-fix-1", "explain-repo", "navigation-ctf", "cross-package", "impact-analysis", "feature-localization", "config-audit", "dead-code", "migration"];
  const targets: TargetName[] = ["aethyme", "grc", "mediawiki"];
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

        <label
          className={[
            "flex items-center gap-1.5 text-xs select-none cursor-pointer px-2 py-1 rounded",
            divergentOnly
              ? "bg-[var(--color-score-yellow)]/15 text-[var(--color-score-yellow)]"
              : "text-[var(--color-text-muted)] hover:text-[var(--color-text)]",
          ].join(" ")}
          title="Show only rows where the LLM judge and keyword scorer disagree by more than the divergence threshold — useful for manually reviewing scoring quality."
        >
          <input
            type="checkbox"
            checked={divergentOnly}
            onChange={(e) => setDivergentOnly(e.target.checked)}
            className="accent-[var(--color-score-yellow)]"
          />
          <span>
            Divergent only
            {divergentCount > 0 && (
              <span className="ml-1 font-mono">({divergentCount})</span>
            )}
          </span>
        </label>

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

      {divergentOnly && filtered.length > 0 && (
        <div className="text-xs text-[var(--color-text-muted)] bg-[var(--color-score-yellow)]/8 border border-[var(--color-score-yellow)]/30 rounded px-3 py-2">
          <span className="text-[var(--color-score-yellow)] font-medium">Review queue.</span>{" "}
          These rows have a large gap between the keyword scorer and the LLM
          judge. That's a data-quality signal, not a scoring axis — no
          automated downstream action is taken. Open the run to see why they
          disagree and decide whether the scorer or the judge needs attention.
        </div>
      )}

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
