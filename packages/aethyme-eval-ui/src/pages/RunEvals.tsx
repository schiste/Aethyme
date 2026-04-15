import { useState, useEffect } from "react";
import {
  fetchRepositories,
  fetchChau7Tabs,
  generatePlan,
  launchRun,
  fetchRunStatus,
  prepareRepository,
  fetchLatestPreparation,
  setupRepository,
  fetchSetupStatus,
} from "../lib/api";
import type {
  EvalType,
  ModelName,
  Reasoning,
  Repository,
  RunStatus,
  RepositoryPreparation,
  RepositorySetupStatus,
} from "../lib/types";

const EVAL_TYPES: { value: EvalType; label: string; description: string; target?: string; conditions: string }[] = [
  {
    value: "bug-fix",
    label: "Bug Fix (GRC)",
    description: "Plants a bug in rbac-canonical.ts (removes Action.SHARE from manage implications). Agent must find and fix it. 4 isolated repo clones.",
    target: "grc",
    conditions: "5 conditions: control-cto-off, control-cto-on, explore (skill only), leverage (generic Aethyme guidance), task-conditioned (engine-generated task guidance)",
  },
  {
    value: "bug-fix-1",
    label: "Bug Fix T419918 (MediaWiki)",
    description: "Real MediaWiki bug — viewing a diff marks all revisions as 'seen'. Agent must identify files to edit and explain the fix without applying it.",
    target: "mediawiki",
    conditions: "5 conditions: control-cto-off, control-cto-on, explore (skill only), leverage (generic Aethyme guidance), task-conditioned (engine-generated task guidance)",
  },
  {
    value: "explain-repo",
    label: "Explain Repo",
    description: "Agent produces a structured explanation of the repository: architecture, entry points, subsystems, testing, frontend, extensions.",
    conditions: "5 conditions: control-cto-off, control-cto-on, explore (skill only), leverage (generic Aethyme guidance), task-conditioned (engine-generated task guidance)",
  },
  {
    value: "navigation-ctf",
    label: "Navigation CTF",
    description: "Directed graph challenge — find the config, entrypoint, and management area linked by graph relations.",
    conditions: "5 conditions: control-cto-off, control-cto-on, explore (skill only), leverage (generic Aethyme guidance), task-conditioned (engine-generated task guidance)",
  },
  {
    value: "cross-package",
    label: "Cross Package (GRC)",
    description: "Removes Action.READ from execute implications. Test in app-shared, bug in packages/auth — 4-hop import chain. Symptom-driven prompt with no file paths.",
    target: "grc",
    conditions: "5 conditions with efficiency-focused scoring (30% weight on efficiency)",
  },
  {
    value: "impact-analysis",
    label: "Impact Analysis (MediaWiki)",
    description: "WikiPage::doViewUpdates() is being refactored. Agent must find all call sites across the codebase that would need updating. Tests graph edge traversal.",
    target: "mediawiki",
    conditions: "Scored by: set overlap of file:line pairs with reference (6 call sites in 3 files)",
  },
  {
    value: "feature-localization",
    label: "Feature Localization (MediaWiki)",
    description: "Trace the 'Watch' button click from HTTP handler to database write. Agent must identify the full 4-step execution chain in order.",
    target: "mediawiki",
    conditions: "Scored by: ordered chain match — each step (file + method) scores 25%",
  },
  {
    value: "config-audit",
    label: "Config Audit (MediaWiki)",
    description: "Find the rate limiting config variable, where it's defined, the enforcement class, and how to disable it. Tests config-to-code tracing.",
    target: "mediawiki",
    conditions: "Scored by: 25% per correct answer (config var, default location, enforcement class, disable method)",
  },
  {
    value: "dead-code",
    label: "Dead Code (MediaWiki)",
    description: "Find public methods in includes/Watchlist/ that are never called from outside that directory.",
    target: "mediawiki",
    conditions: "5 conditions: control-cto-off, control-cto-on, explore (skill only), leverage (generic Aethyme guidance), task-conditioned (engine-generated task guidance)",
  },
  {
    value: "migration",
    label: "Migration (MediaWiki)",
    description: "Find every non-test reference that must change when WatchedItemStore is renamed.",
    target: "mediawiki",
    conditions: "5 conditions: control-cto-off, control-cto-on, explore (skill only), leverage (generic Aethyme guidance), task-conditioned (engine-generated task guidance)",
  },
];

const MODELS: { value: ModelName; label: string; provider: string; backend: string }[] = [
  { value: "haiku", label: "Haiku 4.5", provider: "Anthropic", backend: "Claude Code" },
  { value: "sonnet", label: "Sonnet 4", provider: "Anthropic", backend: "Claude Code" },
  { value: "opus", label: "Opus 4", provider: "Anthropic", backend: "Claude Code" },
  { value: "gpt-5.4", label: "GPT-5.4", provider: "OpenAI", backend: "Codex" },
];

const REASONING: { value: Reasoning; label: string }[] = [
  { value: "high", label: "High" },
  { value: "low", label: "Low" },
];

function statusColor(status: RunStatus): string {
  switch (status) {
    case "idle": return "text-[var(--color-text-muted)]";
    case "planning": return "text-[var(--color-score-yellow)]";
    case "running": return "text-[var(--color-accent)]";
    case "complete": return "text-[var(--color-score-green)]";
    case "error": return "text-[var(--color-score-red)]";
  }
}

export default function RunEvals() {
  const [evalType, setEvalType] = useState<EvalType>("explain-repo");
  const [target, setTarget] = useState<string>("grc");
  const [model, setModel] = useState<ModelName>("haiku");
  const [reasoning, setReasoning] = useState<Reasoning>("high");
  const [windowId, setWindowId] = useState<string>("auto");
  const [cleanupDelaySeconds, setCleanupDelaySeconds] = useState<string>("1");

  const [repos, setRepos] = useState<Repository[]>([]);
  const [loadingRepos, setLoadingRepos] = useState(true);
  const [availableWindows, setAvailableWindows] = useState<number[]>([]);
  const [preparation, setPreparation] = useState<RepositoryPreparation | null>(null);
  const [preparing, setPreparing] = useState(false);
  const [setupStatus, setSetupStatus] = useState<RepositorySetupStatus | null>(null);
  const [settingUp, setSettingUp] = useState(false);
  const [setupForce, setSetupForce] = useState(false);

  const [status, setStatus] = useState<RunStatus>("idle");
  const [plan, setPlan] = useState<object | null>(null);
  const [log, setLog] = useState<string[]>([]);
  const [currentPhase, setCurrentPhase] = useState<string | null>(null);

  const reloadRepositories = async () => {
    const data = await fetchRepositories();
    setRepos(data);
    if (data.length > 0 && !data.find((r) => r.target === target)) {
      setTarget(data[0].target);
    }
    return data;
  };

  useEffect(() => {
    reloadRepositories()
      .then((data) => {
        setRepos(data);
      })
      .catch((err) => {
        setLog((prev) => [...prev, `ERROR: Failed to load repositories: ${err.message}`]);
      })
      .finally(() => setLoadingRepos(false));
  }, []);

  useEffect(() => {
    fetchChau7Tabs()
      .then((tabs) => {
        const windows = Array.from(
          new Set(
            tabs
              .map((tab) => tab?.window_id)
              .filter((value): value is number => Number.isInteger(value)),
          ),
        ).sort((a, b) => a - b);
        setAvailableWindows(windows);
      })
      .catch(() => {
        setAvailableWindows([]);
      });
  }, []);

  useEffect(() => {
    fetchLatestPreparation(target)
      .then((snapshot) => setPreparation(snapshot))
      .catch(() => setPreparation(null));
    setSetupStatus(null);
  }, [target]);

  const selectedEvalType = EVAL_TYPES.find((t) => t.value === evalType);
  const selectedModel = MODELS.find((m) => m.value === model);
  const selectedRepo = repos.find((repo) => repo.target === target) ?? null;
  // Auto-set target when eval type has a restriction
  useEffect(() => {
    if (selectedEvalType?.target) {
      setTarget(selectedEvalType.target);
    }
  }, [evalType]);

  useEffect(() => {
    setPlan(null);
  }, [evalType, target, model, reasoning, windowId, cleanupDelaySeconds]);

  const parsedWindowId =
    windowId === "auto" || windowId === "" ? undefined : Number.parseInt(windowId, 10);
  const parsedCleanupDelaySeconds = Number.parseInt(cleanupDelaySeconds, 10);
  const effectiveCleanupDelaySeconds =
    Number.isFinite(parsedCleanupDelaySeconds) && parsedCleanupDelaySeconds >= 0
      ? parsedCleanupDelaySeconds
      : 1;

  async function handleGeneratePlan() {
    setStatus("planning");
    setPlan(null);
    setLog([]);
    try {
      const config = parsedWindowId === undefined
        ? { evalType, target, model, reasoning, cleanupDelaySeconds: effectiveCleanupDelaySeconds }
        : { evalType, target, model, reasoning, windowId: parsedWindowId, cleanupDelaySeconds: effectiveCleanupDelaySeconds };
      const result = await generatePlan(config);
      setPlan(result);
      setLog((prev) => [...prev, "Plan generated successfully."]);
      setStatus("idle");
    } catch (err: any) {
      setLog((prev) => [...prev, `ERROR: ${err.message}`]);
      setStatus("error");
    }
  }

  async function handlePrepareTarget() {
    setPreparing(true);
    try {
      const snapshot = await prepareRepository(target);
      setPreparation(snapshot);
      setLog((prev) => [
        ...prev,
        snapshot.ready
          ? `Preparation ready: ${snapshot.id}`
          : `Preparation failed: ${snapshot.errors.join(" | ")}`,
      ]);
    } catch (err: any) {
      setLog((prev) => [...prev, `ERROR: ${err.message}`]);
    } finally {
      setPreparing(false);
    }
  }

  async function handleSetupTarget() {
    if (!selectedRepo) {
      setLog((prev) => [...prev, "ERROR: No target repository selected."]);
      return;
    }
    if (!selectedRepo.setupSource || !selectedRepo.setupCommit) {
      setLog((prev) => [...prev, "ERROR: This target has no default setup source/commit configured yet."]);
      return;
    }

    setSettingUp(true);
    setSetupStatus(null);
    setPreparation(null);
    setPlan(null);
    setLog((prev) => [
      ...prev,
      `Starting setup for ${target} from ${selectedRepo.setupSource} @ ${selectedRepo.setupCommit}${setupForce ? " (force)" : ""}`,
    ]);

    try {
      const launch = await setupRepository({
        target,
        source: selectedRepo.setupSource,
        commit: selectedRepo.setupCommit,
        force: setupForce,
      });
      if (!launch.taskId) {
        throw new Error("Setup did not return a task id.");
      }

      const poll = async () => {
        const nextStatus = await fetchSetupStatus(launch.taskId as string);
        setSetupStatus(nextStatus);

        if (nextStatus.status === "complete") {
          setSettingUp(false);
          if (nextStatus.preparation) {
            setPreparation(nextStatus.preparation);
          } else {
            const latest = await fetchLatestPreparation(target);
            setPreparation(latest);
          }
          await reloadRepositories();
          setLog((prev) => [
            ...prev,
            nextStatus.skipped
              ? `Setup skipped for ${target}; playground already ready.`
              : `Setup complete for ${target}.`,
          ]);
          return;
        }

        if (nextStatus.status === "error") {
          setSettingUp(false);
          setLog((prev) => [
            ...prev,
            `ERROR: Setup failed for ${target}: ${nextStatus.error ?? "unknown error"}`,
          ]);
          return;
        }

        window.setTimeout(() => {
          void poll();
        }, 3000);
      };

      await poll();
    } catch (err: any) {
      setSettingUp(false);
      setLog((prev) => [...prev, `ERROR: ${err.message}`]);
    }
  }

  async function handleRun() {
    if (!plan) {
      setLog((prev) => [...prev, "Generate a plan first before running."]);
      return;
    }
    if (!preparation?.ready) {
      setLog((prev) => [...prev, "Prepare the target first before running."]);
      return;
    }
    setStatus("running");
    setLog([]);
    try {
      const config = parsedWindowId === undefined
        ? { evalType, target, model, reasoning, preparationId: preparation.id, cleanupDelaySeconds: effectiveCleanupDelaySeconds }
        : { evalType, target, model, reasoning, windowId: parsedWindowId, preparationId: preparation.id, cleanupDelaySeconds: effectiveCleanupDelaySeconds };
      const state = await launchRun(config);
      setCurrentPhase(state.currentPhase);
      setLog(state.log);
      if (state.status === "error") {
        setStatus("error");
        return;
      }

      // Poll for status updates every 5 seconds
      const pollInterval = setInterval(async () => {
        try {
          const status = await fetchRunStatus();
          setLog([...status.log]);
          setCurrentPhase(status.currentPhase);
          if (status.status === "complete") {
            setStatus("complete");
            clearInterval(pollInterval);
          } else if (status.status === "error") {
            setStatus("error");
            clearInterval(pollInterval);
          }
        } catch {
          // ignore poll errors
        }
      }, 5000);

      // Safety: stop polling after 30 minutes
      setTimeout(() => clearInterval(pollInterval), 30 * 60 * 1000);
    } catch (err: any) {
      setLog((prev) => [...prev, `ERROR: ${err.message}`]);
      setStatus("error");
    }
  }

  const selectClass =
    "w-full bg-[var(--color-surface)] border border-[var(--color-border)] rounded px-3 py-2 text-sm text-[var(--color-text)] focus:outline-none focus:border-[var(--color-accent)]";

  return (
    <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
      {/* Left: Configuration */}
      <div className="space-y-4">
        <div className="bg-[var(--color-surface)] border border-[var(--color-border)] rounded-lg p-5 space-y-4">
          <h2 className="text-sm font-medium text-[var(--color-text-muted)] uppercase tracking-wide">
            Configuration
          </h2>

          <div className="space-y-3">
            <div className="rounded border border-[var(--color-border)] bg-[var(--color-bg)] px-3 py-3 space-y-2">
              <div className="flex items-center justify-between gap-3">
                <div>
                  <p className="text-xs text-[var(--color-text-muted)] uppercase tracking-wide">Setup Playground</p>
                  <p className="text-sm text-[var(--color-text)]">
                    Recreate the control/aethyme playground pair, deploy the skill, and build the Aethyme index.
                  </p>
                </div>
                <button
                  onClick={handleSetupTarget}
                  disabled={settingUp || status === "running" || loadingRepos || !selectedRepo?.setupSource || !selectedRepo?.setupCommit}
                  className="px-4 py-2 text-sm font-medium rounded border border-[var(--color-border)] text-[var(--color-text)] hover:border-[var(--color-accent)] hover:bg-[var(--color-surface-hover)] transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                >
                  {settingUp ? "Setting Up..." : "Setup Target"}
                </button>
              </div>
              {selectedRepo?.setupSource && selectedRepo?.setupCommit ? (
                <div className="space-y-1 text-xs font-mono text-[var(--color-text-muted)]">
                  <div>source: {selectedRepo.setupSource}</div>
                  <div>commit: {selectedRepo.setupCommit}</div>
                  <div>dest: {selectedRepo.setupDest}</div>
                </div>
              ) : (
                <p className="text-xs text-[var(--color-score-yellow)]">
                  No default setup source/commit is configured for this target yet.
                </p>
              )}
              <label className="flex items-center gap-2 text-xs text-[var(--color-text-muted)]">
                <input
                  type="checkbox"
                  checked={setupForce}
                  onChange={(e) => setSetupForce(e.target.checked)}
                  className="rounded border-[var(--color-border)] bg-[var(--color-surface)]"
                />
                Replace any existing playground repos for this target
              </label>
              {setupStatus && (
                <div className={`rounded border px-3 py-2 text-xs ${
                  setupStatus.status === "error"
                    ? "border-[var(--color-score-red)]/40 bg-[var(--color-score-red)]/10"
                    : setupStatus.status === "complete"
                      ? "border-[var(--color-score-green)]/40 bg-[var(--color-score-green)]/10"
                      : "border-[var(--color-accent)]/30 bg-[var(--color-accent)]/10"
                }`}>
                  <div className="font-mono text-[var(--color-text)]">status: {setupStatus.status}</div>
                  {setupStatus.skipped && (
                    <div className="mt-1 text-[var(--color-text-muted)]">Playground already healthy, so setup was skipped.</div>
                  )}
                  {setupStatus.output && (
                    <pre className="mt-2 max-h-28 overflow-y-auto whitespace-pre-wrap font-mono text-[11px] text-[var(--color-text-muted)]">
                      {setupStatus.output}
                    </pre>
                  )}
                  {setupStatus.error && (
                    <div className="mt-2 text-[var(--color-score-red)]">{setupStatus.error}</div>
                  )}
                </div>
              )}
            </div>

            <div>
              <label className="block text-xs text-[var(--color-text-muted)] uppercase tracking-wide mb-1">
                Eval Type
              </label>
              <select
                value={evalType}
                onChange={(e) => setEvalType(e.target.value as EvalType)}
                className={selectClass}
              >
                {EVAL_TYPES.map((t) => (
                  <option key={t.value} value={t.value}>
                    {t.label}
                  </option>
                ))}
              </select>
            </div>

            <div>
              <label className="block text-xs text-[var(--color-text-muted)] uppercase tracking-wide mb-1">
                Target
              </label>
              <select
                value={target}
                onChange={(e) => setTarget(e.target.value)}
                className={selectClass}
                disabled={loadingRepos || !!selectedEvalType?.target}
              >
                {loadingRepos ? (
                  <option>Loading...</option>
                ) : repos.length === 0 ? (
                  <option>No targets available</option>
                ) : (
                  repos.map((r) => (
                    <option key={r.target} value={r.target}>
                      {r.name}
                    </option>
                  ))
                )}
              </select>
              {selectedEvalType?.target && (
                <p className="text-xs text-[var(--color-text-muted)] mt-1">
                  Locked to {selectedEvalType.target} for this eval type
                </p>
              )}
            </div>

            <div className="grid grid-cols-2 gap-3">
              <div>
                <label className="block text-xs text-[var(--color-text-muted)] uppercase tracking-wide mb-1">
                  Model
                </label>
                <select
                  value={model}
                  onChange={(e) => setModel(e.target.value as ModelName)}
                  className={selectClass}
                >
                  {MODELS.map((m) => (
                    <option key={m.value} value={m.value}>
                      {m.label}
                    </option>
                  ))}
                </select>
              </div>

              <div>
                <label className="block text-xs text-[var(--color-text-muted)] uppercase tracking-wide mb-1">
                  Reasoning
                </label>
                <select
                  value={reasoning}
                  onChange={(e) => setReasoning(e.target.value as Reasoning)}
                  className={selectClass}
                >
                  {REASONING.map((r) => (
                    <option key={r.value} value={r.value}>
                      {r.label}
                    </option>
                  ))}
                </select>
              </div>

              <div>
                <label className="block text-xs text-[var(--color-text-muted)] uppercase tracking-wide mb-1">
                  Chau7 Window
                </label>
                <select
                  value={windowId}
                  onChange={(e) => setWindowId(e.target.value)}
                  className={selectClass}
                >
                  <option value="auto">Auto</option>
                  {availableWindows.map((id) => (
                    <option key={id} value={String(id)}>
                      Window {id}
                    </option>
                  ))}
                </select>
              </div>

              <div>
                <label className="block text-xs text-[var(--color-text-muted)] uppercase tracking-wide mb-1">
                  Cleanup Delay
                </label>
                <input
                  type="number"
                  min={0}
                  step={1}
                  value={cleanupDelaySeconds}
                  onChange={(e) => setCleanupDelaySeconds(e.target.value)}
                  className={selectClass}
                />
              </div>
            </div>
            <div className="space-y-1 text-xs text-[var(--color-text-muted)]">
              <p>
                Auto uses Chau7&apos;s active/preferred window. Pick a window ID to force eval tabs into that window.
              </p>
              <p>
                Cleanup delay is the number of seconds to keep eval tabs open after finalization before closing them. Use a long delay for debugging and `1` for normal data collection.
              </p>
            </div>
          </div>

          <div className="flex gap-2 pt-2">
            <button
              onClick={handlePrepareTarget}
              disabled={preparing || status === "running" || loadingRepos || settingUp}
              className="flex-1 px-4 py-2 text-sm font-medium rounded border border-[var(--color-border)] text-[var(--color-text)] hover:border-[var(--color-accent)] hover:bg-[var(--color-surface-hover)] transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
            >
              {preparing ? "Preparing..." : "Prepare Target"}
            </button>
            <button
              onClick={handleGeneratePlan}
              disabled={status === "planning" || status === "running" || loadingRepos || settingUp}
              className="flex-1 px-4 py-2 text-sm font-medium rounded border border-[var(--color-border)] text-[var(--color-text)] hover:border-[var(--color-accent)] hover:bg-[var(--color-surface-hover)] transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
            >
              {status === "planning" ? "Generating..." : "Generate Plan"}
            </button>
            <button
              onClick={handleRun}
              disabled={!plan || status === "running" || !preparation?.ready || settingUp}
              className="flex-1 px-4 py-2 text-sm font-medium rounded bg-[var(--color-accent)] text-white hover:bg-[var(--color-accent-hover)] transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
            >
              {status === "running" ? "Running..." : "Run"}
            </button>
          </div>
          <p className="text-xs text-[var(--color-text-muted)]">
            Flow: setup playground if needed, prepare a readiness snapshot, generate the dry plan, then run the evaluation.
          </p>
        </div>

        {/* Eval type details */}
        {selectedEvalType && (
          <div className="bg-[var(--color-surface)] border border-[var(--color-border)] rounded-lg p-5 space-y-3">
            <h2 className="text-sm font-medium text-[var(--color-text-muted)] uppercase tracking-wide">
              {selectedEvalType.label}
            </h2>
            <p className="text-sm text-[var(--color-text)]">
              {selectedEvalType.description}
            </p>
            <div className="space-y-2 text-xs">
              <div className="flex gap-2">
                <span className="text-[var(--color-text-muted)] uppercase tracking-wide w-20 shrink-0">Conditions</span>
                <span className="text-[var(--color-text)] font-mono">{selectedEvalType.conditions}</span>
              </div>
              {selectedEvalType.target && (
                <div className="flex gap-2">
                  <span className="text-[var(--color-text-muted)] uppercase tracking-wide w-20 shrink-0">Target</span>
                  <span className="text-[var(--color-text)] font-mono">{selectedEvalType.target} only</span>
                </div>
              )}
              {selectedModel && (
                <div className="flex gap-2">
                  <span className="text-[var(--color-text-muted)] uppercase tracking-wide w-20 shrink-0">Backend</span>
                  <span className="text-[var(--color-text)] font-mono">{selectedModel.provider} / {selectedModel.backend}</span>
                </div>
              )}
            </div>
          </div>
        )}

        {/* Plan display */}
        {plan && (
          <div className="bg-[var(--color-surface)] border border-[var(--color-border)] rounded-lg p-5 space-y-3">
            <h2 className="text-sm font-medium text-[var(--color-text-muted)] uppercase tracking-wide">
              Plan
            </h2>
            <pre className="text-xs font-mono text-[var(--color-text)] bg-[var(--color-bg)] rounded p-3 overflow-x-auto max-h-64 overflow-y-auto">
              {JSON.stringify(plan, null, 2)}
            </pre>
          </div>
        )}
      </div>

      {/* Right: Status and log */}
      <div className="space-y-4">
        {/* Repository readiness */}
        <div className="bg-[var(--color-surface)] border border-[var(--color-border)] rounded-lg p-5 space-y-3">
          <h2 className="text-sm font-medium text-[var(--color-text-muted)] uppercase tracking-wide">
            Target Repositories
          </h2>
          {preparation && (
            <div className={`rounded border px-3 py-2 text-xs ${
              preparation.ready
                ? "border-[var(--color-score-green)]/40 bg-[var(--color-score-green)]/10 text-[var(--color-text)]"
                : "border-[var(--color-score-red)]/40 bg-[var(--color-score-red)]/10 text-[var(--color-text)]"
            }`}>
              <div className="font-mono">Preparation {preparation.id}</div>
              <div>{preparation.ready ? "ready" : "not ready"}</div>
              {!preparation.ready && preparation.errors.length > 0 && (
                <div className="mt-1 text-[var(--color-score-red)]">
                  {preparation.errors.join(" | ")}
                </div>
              )}
            </div>
          )}
          {loadingRepos ? (
            <p className="text-xs text-[var(--color-text-muted)]">Loading...</p>
          ) : (
            <div className="space-y-2">
              {repos.map((repo) => (
                <div
                  key={repo.target}
                  className={`flex items-center justify-between p-2.5 rounded border ${
                    repo.target === target
                      ? "border-[var(--color-accent)]/50 bg-[var(--color-accent)]/5"
                      : "border-[var(--color-border)]"
                  }`}
                >
                  <div className="flex items-center gap-3">
                    <span className={`w-2 h-2 rounded-full ${
                      repo.validationStatus === "valid"
                        ? "bg-[var(--color-score-green)]"
                        : "bg-[var(--color-score-red)]"
                    }`} />
                    <div>
                      <span className="text-sm font-medium text-[var(--color-text)]">{repo.name}</span>
                      {repo.target === target && (
                        <span className="ml-2 text-xs text-[var(--color-accent)]">selected</span>
                      )}
                      <p className="text-xs font-mono text-[var(--color-text-muted)]">
                        {repo.controlPath}
                      </p>
                    </div>
                  </div>
                  <div className="text-right">
                    {repo.aethymeIndex?.indexed ? (
                      <div>
                        <span className="text-xs font-mono text-[var(--color-score-green)]">indexed</span>
                        <p className="text-xs text-[var(--color-text-muted)] font-mono">
                          {new Date(repo.aethymeIndex.date || "").toLocaleDateString("en-US", {
                            month: "short", day: "numeric", hour: "2-digit", minute: "2-digit",
                          })}
                        </p>
                      </div>
                    ) : (
                      <span className="text-xs font-mono text-[var(--color-score-yellow)]">no index</span>
                    )}
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>

        {/* Run status */}
        <div className="bg-[var(--color-surface)] border border-[var(--color-border)] rounded-lg p-5 space-y-3">
          <h2 className="text-sm font-medium text-[var(--color-text-muted)] uppercase tracking-wide">
            Run Status
          </h2>
          <div className="flex items-center gap-3">
            <span className={`text-sm font-mono font-semibold ${statusColor(status)}`}>
              {status.toUpperCase()}
            </span>
            {currentPhase && status === "running" && (
              <span className="text-xs text-[var(--color-text-muted)]">
                Phase: {currentPhase}
              </span>
            )}
          </div>
          {status === "running" && (
            <div className="h-1 rounded-full bg-[var(--color-border)] overflow-hidden">
              <div className="h-full bg-[var(--color-accent)] rounded-full w-1/3 animate-pulse" />
            </div>
          )}
        </div>

        {/* Log */}
        <div className="bg-[var(--color-surface)] border border-[var(--color-border)] rounded-lg p-5 space-y-3">
          <h2 className="text-sm font-medium text-[var(--color-text-muted)] uppercase tracking-wide">
            Log
          </h2>
          <div className="bg-[var(--color-bg)] rounded p-3 min-h-32 max-h-96 overflow-y-auto">
            {log.length === 0 ? (
              <p className="text-xs text-[var(--color-text-muted)] font-mono">
                No activity yet. Setup or prepare the target, then generate a plan to get started.
              </p>
            ) : (
              <div className="space-y-1">
                {log.map((entry, i) => (
                  <p
                    key={i}
                    className={`text-xs font-mono ${
                      entry.startsWith("ERROR")
                        ? "text-[var(--color-score-red)]"
                        : "text-[var(--color-text)]"
                    }`}
                  >
                    <span className="text-[var(--color-text-muted)] mr-2">
                      [{String(i + 1).padStart(2, "0")}]
                    </span>
                    {entry}
                  </p>
                ))}
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
