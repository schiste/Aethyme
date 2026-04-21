import type { Repository } from "../lib/types";
import {
  evaluateReadiness,
  projectCost,
  type CheckStatus,
  type CostProjection,
} from "../lib/readiness";

interface Props {
  repo: Repository | null;
  model: string;
  evalType: string;
  runs: number;
  useJudge: boolean;
  judgeSamples: number;
}

const STATUS_COLOR: Record<CheckStatus, string> = {
  ok: "text-[var(--color-score-green)]",
  warning: "text-[var(--color-score-yellow)]",
  blocker: "text-[var(--color-score-red)]",
  unknown: "text-[var(--color-text-muted)]",
};

const STATUS_DOT: Record<CheckStatus, string> = {
  ok: "bg-[var(--color-score-green)]",
  warning: "bg-[var(--color-score-yellow)]",
  blocker: "bg-[var(--color-score-red)]",
  unknown: "bg-[var(--color-border)]",
};

const STATUS_LABEL: Record<CheckStatus, string> = {
  ok: "Ready to launch",
  warning: "Launch with caution",
  blocker: "Not ready — resolve blockers",
  unknown: "Readiness unknown",
};

function confidenceNote(c: CostProjection["confidence"]): string {
  switch (c) {
    case "anchored": return "based on recent runs of this (model, eval type)";
    case "fallback": return `using ${"model"}-level default — actual may vary`;
    case "guess":    return "no calibration for this model — rough estimate";
  }
}

export default function ReadinessPanel(props: Props) {
  const readiness = evaluateReadiness(props.repo);
  const cost = projectCost({
    model: props.model,
    evalType: props.evalType,
    runs: props.runs,
    useJudge: props.useJudge,
    judgeSamples: props.judgeSamples,
  });

  return (
    <div className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface)] p-4 space-y-3">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <span className={`inline-block w-2.5 h-2.5 rounded-full ${STATUS_DOT[readiness.overall]}`} />
          <span className={`text-sm font-semibold ${STATUS_COLOR[readiness.overall]}`}>
            {STATUS_LABEL[readiness.overall]}
          </span>
          {props.repo && (
            <span className="text-xs font-mono text-[var(--color-text-muted)]">
              · {props.repo.target}
            </span>
          )}
        </div>
        <div className="text-right">
          <div className="text-xs text-[var(--color-text-muted)]">projected cost</div>
          <div className="font-mono font-semibold text-[var(--color-text)]">
            ${cost.totalUsd.toFixed(2)}
          </div>
        </div>
      </div>

      <div className="grid grid-cols-2 md:grid-cols-4 gap-2">
        {readiness.checks.map((check) => (
          <div
            key={check.key}
            className="flex items-start gap-2 text-xs"
            title={check.detail}
          >
            <span className={`inline-block w-1.5 h-1.5 rounded-full mt-1 flex-none ${STATUS_DOT[check.status]}`} />
            <div className="flex-1 min-w-0">
              <div className={`font-medium ${STATUS_COLOR[check.status]} truncate`}>
                {check.label}
              </div>
              <div className="text-[var(--color-text-muted)] text-[11px] truncate">
                {check.detail}
              </div>
            </div>
          </div>
        ))}
      </div>

      <div className="border-t border-[var(--color-border)] pt-2 text-[11px] text-[var(--color-text-muted)] flex flex-wrap gap-x-4 gap-y-1">
        <span>
          Agent: <span className="font-mono text-[var(--color-text)]">${cost.agentCostUsd.toFixed(2)}</span>
        </span>
        {props.useJudge && (
          <span>
            Judge: <span className="font-mono text-[var(--color-text)]">${cost.judgeCostUsd.toFixed(2)}</span>
          </span>
        )}
        <span className="italic">({confidenceNote(cost.confidence)})</span>
      </div>
    </div>
  );
}
