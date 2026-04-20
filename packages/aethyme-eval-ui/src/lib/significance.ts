// ---------------------------------------------------------------------------
// Significance annotation
//
// Turns a raw "condition vs control" delta into a verdict that respects
// pre-registration. The minimum-meaningful delta (MMD) is declared at launch
// time in the RunRequest; we load it from any row in the pair (values are
// identical across conditions in one batch).
//
// Verdicts:
//   "meaningful"    — |delta| >= MMD
//   "within-noise"  — |delta| < MMD (smaller than what we pre-declared as real)
//   "insufficient"  — we don't have enough runs to make the call (n < 2 on
//                     either side of the comparison, so no stdev)
//   null            — one side is missing a score entirely
//
// We do NOT perform a full t-test or CI computation here; the user's protocol
// treats MMD as the decision threshold by design. Future extensions could add
// Welch's t-test on top of this same shape. Keep this function pure.
// ---------------------------------------------------------------------------

export type Verdict = "meaningful" | "within-noise" | "insufficient";

export interface SignificanceInput {
  /** Treatment arm (e.g. explore, leverage). null means missing data. */
  treatment: number | null | undefined;
  /** Control arm (e.g. control-cto-off). null means missing data. */
  control: number | null | undefined;
  /** How many repetitions make up `treatment`. */
  treatmentN: number;
  /** How many repetitions make up `control`. */
  controlN: number;
  /** Pre-registered minimum meaningful delta. Falsy → defaults to 5.0
   * (matches the server-side default when no pre-reg was declared). */
  minimumMeaningfulDelta: number | null | undefined;
  /** Higher is better? Quality=true, Tokens/Cost=false. Controls sign. */
  higherIsBetter: boolean;
}

export interface SignificanceResult {
  verdict: Verdict | null;
  /** Signed delta (treatment - control); positive means treatment wins
   * when higherIsBetter, or treatment is cheaper when !higherIsBetter. */
  delta: number | null;
  /** The MMD that was applied — exposed so the UI can explain the verdict. */
  mmdApplied: number;
  /** Whether the MMD was user-declared (true) or defaulted (false). */
  mmdPreRegistered: boolean;
}

export const DEFAULT_MMD = 5.0;

export function annotate(input: SignificanceInput): SignificanceResult {
  const { treatment, control, treatmentN, controlN, higherIsBetter } = input;
  const preReg = typeof input.minimumMeaningfulDelta === "number" && input.minimumMeaningfulDelta > 0;
  const mmd = preReg ? input.minimumMeaningfulDelta! : DEFAULT_MMD;

  if (treatment == null || control == null) {
    return { verdict: null, delta: null, mmdApplied: mmd, mmdPreRegistered: preReg };
  }

  const rawDelta = treatment - control;
  // Flip sign when lower is better so "positive delta = win" invariant holds.
  const signedDelta = higherIsBetter ? rawDelta : -rawDelta;

  if (treatmentN < 2 || controlN < 2) {
    // A single run each side gives us a point estimate but no noise floor.
    // Still useful to report the delta, but don't claim significance.
    if (Math.abs(rawDelta) >= mmd) {
      return { verdict: "insufficient", delta: signedDelta, mmdApplied: mmd, mmdPreRegistered: preReg };
    }
    return { verdict: "insufficient", delta: signedDelta, mmdApplied: mmd, mmdPreRegistered: preReg };
  }

  const verdict: Verdict = Math.abs(rawDelta) >= mmd ? "meaningful" : "within-noise";
  return { verdict, delta: signedDelta, mmdApplied: mmd, mmdPreRegistered: preReg };
}

/** Map a verdict to a tailwind text-color token and a short label, so
 * cell rendering stays consistent across Matrix and ResultsTable. */
export function verdictStyle(v: Verdict | null, signedDelta: number | null): {
  color: string;
  label: string;
} {
  if (v === null) return { color: "text-[var(--color-text-muted)]", label: "—" };
  if (v === "insufficient") return { color: "text-[var(--color-text-muted)]", label: "n<2" };
  if (v === "within-noise") return { color: "text-[var(--color-text-muted)]", label: "≈" };
  // v === "meaningful"
  const win = typeof signedDelta === "number" && signedDelta > 0;
  return {
    color: win ? "text-[var(--color-score-green)]" : "text-[var(--color-score-red)]",
    label: win ? "▲" : "▼",
  };
}
