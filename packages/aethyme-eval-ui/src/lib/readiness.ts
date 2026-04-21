import type { Repository, IndexInfo, SnippetsInfo } from "./types";

// ---------------------------------------------------------------------------
// Readiness checks — "is this repo ready to launch an eval against?"
//
// The server already exposes this data via /api/repositories; we don't want
// to duplicate validation logic, just summarize it for the RunEvals page so
// a user doesn't launch a 10-minute batch against a contaminated control.
// ---------------------------------------------------------------------------

export type CheckStatus = "ok" | "warning" | "blocker" | "unknown";

export interface ReadinessCheck {
  key: string;
  label: string;
  status: CheckStatus;
  detail: string;
}

export interface Readiness {
  overall: CheckStatus;
  checks: ReadinessCheck[];
  /** True iff no blockers — launching is safe. Warnings don't block. */
  canLaunch: boolean;
}

const SEVEN_DAYS_MS = 7 * 24 * 60 * 60 * 1000;

function ageMs(iso: string | null | undefined): number | null {
  if (!iso) return null;
  const t = new Date(iso).getTime();
  if (Number.isNaN(t)) return null;
  return Date.now() - t;
}

function formatAge(ms: number): string {
  const days = Math.floor(ms / (24 * 60 * 60 * 1000));
  if (days >= 1) return `${days}d ago`;
  const hours = Math.floor(ms / (60 * 60 * 1000));
  if (hours >= 1) return `${hours}h ago`;
  const mins = Math.floor(ms / (60 * 1000));
  return `${mins}m ago`;
}

function checkControlClean(repo: Repository): ReadinessCheck {
  if (repo.controlClean.clean) {
    return {
      key: "control-clean",
      label: "Control repo clean",
      status: "ok",
      detail: "No Aethyme contamination detected.",
    };
  }
  return {
    key: "control-clean",
    label: "Control repo contaminated",
    status: "blocker",
    detail: repo.controlClean.issues.join(" · ") || "Unknown contamination.",
  };
}

function checkIndex(info: IndexInfo): ReadinessCheck {
  if (!info.indexed) {
    return {
      key: "index",
      label: "Aethyme graph index missing",
      status: "blocker",
      detail: `No graph.db at ${info.path}. Run the indexer before launching.`,
    };
  }
  const age = ageMs(info.date);
  if (age !== null && age > SEVEN_DAYS_MS) {
    return {
      key: "index",
      label: "Aethyme graph index stale",
      status: "warning",
      detail: `Built ${formatAge(age)} — consider re-indexing for recent changes.`,
    };
  }
  return {
    key: "index",
    label: "Aethyme graph index ready",
    status: "ok",
    detail: age !== null ? `Built ${formatAge(age)}, ${info.sizeMb ?? "?"} MB.` : "Present.",
  };
}

function checkSnippets(info: SnippetsInfo): ReadinessCheck {
  if (!info.present) {
    return {
      key: "snippets",
      label: "Chau7 snippets missing",
      status: "warning",
      detail: "Navigation tools will lack pre-generated snippets. Runs still work.",
    };
  }
  return {
    key: "snippets",
    label: "Chau7 snippets present",
    status: "ok",
    detail: `${info.aethymeSnippets}/${info.totalSnippets} tagged aethyme.`,
  };
}

function checkValidation(repo: Repository): ReadinessCheck {
  if (repo.validationStatus === "valid") {
    return { key: "validation", label: "Repo validated", status: "ok", detail: "Passed structural checks." };
  }
  if (repo.validationStatus === "invalid") {
    return { key: "validation", label: "Repo invalid", status: "blocker", detail: "Structural checks failed." };
  }
  if (repo.validationStatus === "checking") {
    return { key: "validation", label: "Validation in progress", status: "unknown", detail: "Running checks…" };
  }
  return {
    key: "validation",
    label: "Validation unknown",
    status: "warning",
    detail: "No recent validation — run validate to confirm readiness.",
  };
}

export function evaluateReadiness(repo: Repository | null): Readiness {
  if (!repo) {
    return {
      overall: "unknown",
      canLaunch: false,
      checks: [
        {
          key: "no-repo",
          label: "No repository selected",
          status: "unknown",
          detail: "Pick a target to see readiness.",
        },
      ],
    };
  }
  const checks = [
    checkValidation(repo),
    checkControlClean(repo),
    checkIndex(repo.aethymeIndex),
    checkSnippets(repo.snippets),
  ];
  const hasBlocker = checks.some((c) => c.status === "blocker");
  const hasWarning = checks.some((c) => c.status === "warning");
  const allOk = checks.every((c) => c.status === "ok");
  const overall: CheckStatus = hasBlocker ? "blocker" : hasWarning ? "warning" : allOk ? "ok" : "unknown";
  return {
    overall,
    canLaunch: !hasBlocker,
    checks,
  };
}

// ---------------------------------------------------------------------------
// Cost projection — rough, intentionally conservative
//
// Real cost depends on the task; we use historical per-condition averages
// by (model, eval_type-family). These are calibration anchors, not
// guarantees. The projection exists to prevent "launch a batch, come back
// two hours later, find out you just spent $60" surprises.
// ---------------------------------------------------------------------------

/** Baseline per-condition USD estimates, anchored to recent Haiku runs in MEMORY.md.
 * `unknown` covers newer eval types we haven't measured yet. */
const PER_CONDITION_USD: Record<string, Record<string, number>> = {
  haiku: {
    "bug-fix":             0.35,
    "bug-fix-1":           0.45,
    "explain-repo":        0.50,
    "dead-code":           0.30,
    "navigation-ctf":      0.25,
    "impact-analysis":     0.30,
    "feature-localization":0.30,
    "config-audit":        0.30,
    "migration":           0.30,
    "cross-package":       0.40,
    unknown:               0.40,
  },
  sonnet: {
    unknown: 1.50, // ~4x Haiku based on per-token pricing
  },
  opus: {
    unknown: 7.00, // ~20x Haiku
  },
  "gpt-5.4": {
    unknown: 0.50, // roughly similar to Haiku for these tasks
  },
};

/** Approximate USD per judge sample. Codex is the only judge backend today. */
const PER_JUDGE_SAMPLE_USD = 0.02;

export const DEFAULT_CONDITIONS_PER_RUN = 5;

export interface CostProjectionInput {
  model: string;
  evalType: string;
  runs: number;
  useJudge: boolean;
  judgeSamples: number;
  /** Override for the "how many conditions per run" count. Default 5. */
  conditionsPerRun?: number;
}

export interface CostProjection {
  agentCostUsd: number;
  judgeCostUsd: number;
  totalUsd: number;
  /** "anchored" when we have a per-model×eval-type estimate,
   * "fallback" when we used the model-level default, "guess" otherwise. */
  confidence: "anchored" | "fallback" | "guess";
}

export function projectCost(input: CostProjectionInput): CostProjection {
  const {
    model, evalType, runs, useJudge, judgeSamples,
    conditionsPerRun = DEFAULT_CONDITIONS_PER_RUN,
  } = input;

  const modelTable = PER_CONDITION_USD[model];
  let perCondition: number;
  let confidence: CostProjection["confidence"];
  if (modelTable && typeof modelTable[evalType] === "number") {
    perCondition = modelTable[evalType];
    confidence = "anchored";
  } else if (modelTable && typeof modelTable.unknown === "number") {
    perCondition = modelTable.unknown;
    confidence = "fallback";
  } else {
    perCondition = 0.50;
    confidence = "guess";
  }

  const agentCostUsd = perCondition * conditionsPerRun * Math.max(1, runs);
  const judgeCostUsd = useJudge
    ? PER_JUDGE_SAMPLE_USD * Math.max(0, judgeSamples) * conditionsPerRun * Math.max(1, runs)
    : 0;

  return {
    agentCostUsd: round(agentCostUsd, 2),
    judgeCostUsd: round(judgeCostUsd, 2),
    totalUsd: round(agentCostUsd + judgeCostUsd, 2),
    confidence,
  };
}

function round(n: number, digits: number): number {
  const f = Math.pow(10, digits);
  return Math.round(n * f) / f;
}
