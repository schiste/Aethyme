import { useState, useEffect, useRef } from "react";
import {
  fetchRepositories,
  indexRepository,
  checkIndexStatus,
} from "../lib/api";
import type { Repository, ValidationStatus } from "../lib/types";

function statusBadge(status: ValidationStatus) {
  const styles: Record<ValidationStatus, string> = {
    valid:
      "bg-[var(--color-score-green)]/15 text-[var(--color-score-green)]",
    invalid:
      "bg-[var(--color-score-red)]/15 text-[var(--color-score-red)]",
    unknown:
      "bg-[var(--color-border)]/50 text-[var(--color-text-muted)]",
    checking:
      "bg-[var(--color-score-yellow)]/15 text-[var(--color-score-yellow)]",
  };
  return (
    <span
      className={`text-xs font-mono px-2 py-0.5 rounded ${styles[status]}`}
    >
      {status}
    </span>
  );
}

function formatDate(iso: string | null): string {
  if (!iso) return "Never";
  const d = new Date(iso);
  return d.toLocaleDateString("en-US", {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function RepoCard({ repo: initial }: { repo: Repository }) {
  const [repo, setRepo] = useState(initial);
  const [indexing, setIndexing] = useState(false);
  const [indexMessage, setIndexMessage] = useState<string | null>(null);
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  useEffect(() => {
    return () => {
      if (pollRef.current) clearInterval(pollRef.current);
    };
  }, []);

  async function handleIndex() {
    setIndexing(true);
    setIndexMessage(null);
    try {
      const result = await indexRepository(repo.target);
      if (result.taskId) {
        pollRef.current = setInterval(async () => {
          try {
            const status = await checkIndexStatus(result.taskId!);
            if (status.status === "complete") {
              setIndexing(false);
              setIndexMessage("Indexing complete");
              setRepo((r) => ({
                ...r,
                lastIndexed: new Date().toISOString(),
              }));
              if (pollRef.current) clearInterval(pollRef.current);
            } else if (status.status === "error") {
              setIndexing(false);
              setIndexMessage(`Error: ${status.error}`);
              if (pollRef.current) clearInterval(pollRef.current);
            }
          } catch {
            setIndexing(false);
            setIndexMessage("Failed to check index status");
            if (pollRef.current) clearInterval(pollRef.current);
          }
        }, 2000);
      }
    } catch (err: any) {
      setIndexing(false);
      setIndexMessage(`Error: ${err.message}`);
    }
  }

  return (
    <div className="bg-[var(--color-surface)] border border-[var(--color-border)] rounded-lg p-5 space-y-4">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <h3 className="text-base font-semibold">{repo.name}</h3>
          <span className="text-xs font-mono px-1.5 py-0.5 rounded bg-[var(--color-accent)]/15 text-[var(--color-accent)]">
            {repo.target}
          </span>
        </div>
        {statusBadge(repo.validationStatus)}
      </div>

      {/* Paths */}
      <div className="space-y-2 text-sm">
        <div>
          <span className="text-[var(--color-text-muted)] text-xs uppercase tracking-wide">
            Control
          </span>
          <p className="font-mono text-xs text-[var(--color-text)] mt-0.5 truncate">
            {repo.controlPath}
          </p>
        </div>
        <div>
          <span className="text-[var(--color-text-muted)] text-xs uppercase tracking-wide">
            Aethyme
          </span>
          <p className="font-mono text-xs text-[var(--color-text)] mt-0.5 truncate">
            {repo.aethymePath}
          </p>
        </div>
      </div>

      {/* Control contamination check */}
      <div className="space-y-1">
        <span className="text-[var(--color-text-muted)] text-xs uppercase tracking-wide">Control Repo</span>
        <div className="p-2.5 rounded border border-[var(--color-border)] text-xs">
          {repo.controlClean.clean ? (
            <div className="flex items-center gap-2">
              <span className="w-1.5 h-1.5 rounded-full bg-[var(--color-score-green)]" />
              <span className="font-mono text-[var(--color-text)]">Clean — no Aethyme contamination</span>
            </div>
          ) : (
            <div className="space-y-1">
              <div className="flex items-center gap-2">
                <span className="w-1.5 h-1.5 rounded-full bg-[var(--color-score-red)]" />
                <span className="font-mono text-[var(--color-score-red)]">Contamination detected</span>
              </div>
              {repo.controlClean.issues.map((issue, i) => (
                <p key={i} className="font-mono text-[var(--color-score-red)] pl-4">{issue}</p>
              ))}
            </div>
          )}
        </div>
      </div>

      {/* Index status */}
      <div className="space-y-1">
        <span className="text-[var(--color-text-muted)] text-xs uppercase tracking-wide">Aethyme Graph Index</span>
        <div className="p-2.5 rounded border border-[var(--color-border)] text-xs">
          {repo.aethymeIndex.indexed ? (
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-2">
                <span className="w-1.5 h-1.5 rounded-full bg-[var(--color-score-green)]" />
                <span className="font-mono text-[var(--color-text)]">Indexed</span>
                {repo.aethymeIndex.sizeMb !== undefined && (
                  <span className="text-[var(--color-text-muted)]">{repo.aethymeIndex.sizeMb} MB</span>
                )}
              </div>
              <span className="font-mono text-[var(--color-text-muted)]">{formatDate(repo.aethymeIndex.date)}</span>
            </div>
          ) : (
            <div className="flex items-center gap-2">
              <span className="w-1.5 h-1.5 rounded-full bg-[var(--color-score-red)]" />
              <span className="font-mono text-[var(--color-text-muted)]">Not indexed — run Re-index</span>
            </div>
          )}
        </div>
      </div>

      {/* Snippets status */}
      <div className="space-y-1">
        <span className="text-[var(--color-text-muted)] text-xs uppercase tracking-wide">Chau7 Snippets</span>
        <div className="p-2.5 rounded border border-[var(--color-border)] text-xs">
          {repo.snippets.present ? (
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-2">
                <span className="w-1.5 h-1.5 rounded-full bg-[var(--color-score-green)]" />
                <span className="font-mono text-[var(--color-text)]">
                  {repo.snippets.aethymeSnippets} Aethyme snippets
                </span>
                {repo.snippets.totalSnippets > repo.snippets.aethymeSnippets && (
                  <span className="text-[var(--color-text-muted)]">
                    ({repo.snippets.totalSnippets} total)
                  </span>
                )}
              </div>
              <span className="font-mono text-[var(--color-text-muted)]">{formatDate(repo.snippets.date)}</span>
            </div>
          ) : (
            <div className="flex items-center gap-2">
              <span className="w-1.5 h-1.5 rounded-full bg-[var(--color-score-red)]" />
              <span className="font-mono text-[var(--color-text-muted)]">No snippets generated — run Re-index</span>
            </div>
          )}
        </div>
      </div>

      {/* Index message */}
      {indexMessage && (
        <p className={`text-xs font-mono ${indexMessage.startsWith("Error") ? "text-[var(--color-score-red)]" : "text-[var(--color-score-green)]"}`}>
          {indexMessage}
        </p>
      )}

      {/* Actions */}
      <div className="flex gap-2 pt-1">
        <button
          onClick={handleIndex}
          disabled={indexing}
          className="px-3 py-1.5 text-xs font-medium rounded border border-[var(--color-border)] text-[var(--color-text-muted)] hover:text-[var(--color-text)] hover:border-[var(--color-accent)] transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
        >
          {indexing ? "Indexing..." : "Re-index"}
        </button>
      </div>
    </div>
  );
}

export default function Repositories() {
  const [repos, setRepos] = useState<Repository[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    fetchRepositories()
      .then((data) => {
        setRepos(data);
        setLoading(false);
      })
      .catch((err) => {
        setError(err.message);
        setLoading(false);
      });
  }, []);

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h2 className="text-sm font-medium text-[var(--color-text-muted)] uppercase tracking-wide">
          Registered Targets
        </h2>
        <span className="text-xs font-mono text-[var(--color-text-muted)]">
          {repos.length} repositories
        </span>
      </div>

      {loading ? (
        <div className="text-center py-12 text-[var(--color-text-muted)]">Loading repositories...</div>
      ) : error ? (
        <div className="text-center py-12 text-[var(--color-score-red)]">
          <p className="text-lg mb-2">Failed to load repositories</p>
          <p className="text-sm font-mono">{error}</p>
          <p className="text-sm mt-2 text-[var(--color-text-muted)]">Make sure the backend server is running on port 8420.</p>
        </div>
      ) : repos.length === 0 ? (
        <div className="text-center py-12 text-[var(--color-text-muted)]">
          <p className="text-lg mb-2">No repositories configured</p>
          <p className="text-sm">Register targets in src/eval/targets.py to see them here.</p>
        </div>
      ) : (
        <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
          {repos.map((repo: Repository) => (
            <RepoCard key={repo.target} repo={repo} />
          ))}
        </div>
      )}
    </div>
  );
}
