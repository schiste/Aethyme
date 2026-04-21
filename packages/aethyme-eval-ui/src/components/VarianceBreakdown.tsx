import type { VarianceComponents } from "../lib/api";

// Visual weights for the three variance bars. Normalized per-batch so the
// longest bar is 100% of track width — we care about *relative* share
// between signal and noise, not absolute magnitude.
function normalizeBars(vc: VarianceComponents): Array<{
  key: "cond" | "run" | "judge";
  label: string;
  value: number | null;
  share: number;
  color: string;
  description: string;
}> {
  const bars = [
    {
      key: "cond" as const,
      label: "σ²_cond (signal)",
      value: vc.sigma2_cond,
      color: "var(--color-score-green, #22c55e)",
      description: "Condition effect — the difference we're trying to detect.",
    },
    {
      key: "run" as const,
      label: "σ²_run (noise)",
      value: vc.sigma2_run,
      color: "var(--color-score-yellow, #eab308)",
      description:
        "Run-to-run variance at fixed (condition, scenario). Reducible with more runs.",
    },
    {
      key: "judge" as const,
      label: "σ²_judge (noise)",
      value: vc.sigma2_judge,
      color: "var(--color-score-red, #ef4444)",
      description:
        "Judge intra-rater variance. Reducible with more samples or a better judge.",
    },
  ];
  const max = Math.max(
    ...bars.map((b) => (b.value !== null && b.value !== undefined ? b.value : 0)),
  );
  return bars.map((b) => ({
    ...b,
    share: b.value && max > 0 ? b.value / max : 0,
  }));
}

function reliabilityLabel(coeff: number | null): {
  text: string;
  color: string;
} {
  if (coeff === null) {
    return { text: "—", color: "var(--color-text-muted)" };
  }
  if (coeff >= 0.8) {
    return {
      text: `${coeff.toFixed(2)} · reliable`,
      color: "var(--color-score-green, #22c55e)",
    };
  }
  if (coeff >= 0.6) {
    return {
      text: `${coeff.toFixed(2)} · usable`,
      color: "var(--color-score-yellow, #eab308)",
    };
  }
  return {
    text: `${coeff.toFixed(2)} · unreliable`,
    color: "var(--color-score-red, #ef4444)",
  };
}

interface Props {
  variance: VarianceComponents | undefined;
}

/**
 * Noise breakdown panel. Reads the variance_components block from
 * /api/batches/{id} and renders three bars (cond / run / judge) plus the
 * generalizability coefficient and the "N runs needed for target" design
 * recommendation.
 */
export default function VarianceBreakdown({ variance }: Props) {
  if (!variance || variance.sigma2_cond === null) {
    return (
      <div className="text-xs text-[var(--color-text-muted)]">
        Variance decomposition unavailable (need ≥2 conditions and &gt;1 row per
        condition).
      </div>
    );
  }

  const bars = normalizeBars(variance);
  const reliability = reliabilityLabel(variance.generalizability_coeff);

  return (
    <div className="space-y-3 text-xs">
      <div className="flex items-center justify-between">
        <h3 className="font-semibold text-sm">Noise breakdown</h3>
        <span className="text-[var(--color-text-muted)]">
          N_rows = {variance.n_rows} · N_conditions = {variance.n_conditions} ·
          mean runs/cond = {variance.mean_runs_per_condition}
        </span>
      </div>

      <div className="space-y-2">
        {bars.map((b) => (
          <div key={b.key} className="space-y-0.5">
            <div className="flex items-baseline justify-between">
              <span className="font-mono">{b.label}</span>
              <span className="font-mono">
                {b.value !== null ? b.value.toFixed(2) : "—"}
              </span>
            </div>
            <div className="h-1.5 bg-[var(--color-surface-subtle,rgba(0,0,0,0.05))] rounded overflow-hidden">
              <div
                className="h-full"
                style={{
                  width: `${Math.max(1, b.share * 100)}%`,
                  backgroundColor: b.color,
                }}
                aria-label={`${b.label} bar ${b.value}`}
              />
            </div>
            <p className="text-[11px] text-[var(--color-text-muted)]">
              {b.description}
            </p>
          </div>
        ))}
      </div>

      <div className="grid grid-cols-2 gap-3 pt-1 border-t border-[var(--color-border)]">
        <div>
          <div className="text-[var(--color-text-muted)] uppercase tracking-wide">
            Generalizability (E-ρ²)
          </div>
          <div className="font-mono" style={{ color: reliability.color }}>
            {reliability.text}
          </div>
          <div className="text-[11px] text-[var(--color-text-muted)]">
            Analog of Cronbach's α for the scorecard. ≥0.80 = conditions
            reliably distinguishable; &lt;0.60 = too noisy to compare.
          </div>
        </div>

        <div>
          <div className="text-[var(--color-text-muted)] uppercase tracking-wide">
            Runs needed for target {variance.target_reliability.toFixed(2)}
          </div>
          <div className="font-mono">
            {variance.n_runs_needed_for_target !== null
              ? `N = ${variance.n_runs_needed_for_target}`
              : "unreachable"}
          </div>
          <div className="text-[11px] text-[var(--color-text-muted)]">
            How many runs per condition are needed to hit the target
            reliability at the observed noise levels. "unreachable" means
            condition signal is below the noise floor — add discriminating
            scenarios or reduce σ²_judge instead.
          </div>
        </div>
      </div>

      {variance.dominant_noise_source && (
        <div className="text-[11px]">
          <span className="text-[var(--color-text-muted)]">
            Dominant noise source:{" "}
          </span>
          <strong>{variance.dominant_noise_source}</strong>{" "}
          <span className="text-[var(--color-text-muted)]">
            ({variance.dominant_noise_source === "judge"
              ? "invest in more judge samples, a better judge, or an ensemble"
              : "invest in more runs per condition"})
          </span>
        </div>
      )}

      {variance.sigma2_cond_clamped_to_zero && (
        <div
          className="text-[11px] p-2 rounded"
          style={{
            backgroundColor: "var(--color-warning-bg, rgba(234,179,8,0.12))",
            color: "var(--color-warning, #b45309)",
          }}
        >
          ⚠ σ²_cond estimate clamped to 0 — MS_between &lt; MS_within. The
          scenario is not discriminating conditions in this batch. Adding runs
          will not fix this; reconsider the scenario or increase sample
          diversity.
        </div>
      )}

      {variance.notes.length > 0 && (
        <ul className="text-[11px] text-[var(--color-text-muted)] list-disc list-inside space-y-0.5">
          {variance.notes.map((n, i) => (
            <li key={i}>{n}</li>
          ))}
        </ul>
      )}
    </div>
  );
}
