// ---------------------------------------------------------------------------
// Five-number summary + outlier detection for box plots.
//
// Uses linear interpolation between data points (NumPy method='linear' /
// R type=7 — the most common default). Below n=2 we don't have enough
// data to compute quartiles, so we return null and let the caller
// degrade gracefully (render a dot instead of a box).
// ---------------------------------------------------------------------------

export interface FiveNumber {
  min: number;
  q1: number;
  median: number;
  q3: number;
  max: number;
  n: number;
}

export interface BoxSummary {
  /** Five-number summary computed from all values (not trimmed). */
  five: FiveNumber;
  /** Whiskers extend from `whiskerLo` to `whiskerHi` — the min/max of
   * values inside 1.5×IQR of the quartiles. Points beyond become outliers. */
  whiskerLo: number;
  whiskerHi: number;
  /** Individual values that fall outside the whiskers. */
  outliers: number[];
  /** Values used to compute the summary (sorted ascending). */
  values: number[];
}

/** Linear-interpolating quantile (R type 7). p ∈ [0, 1]. */
export function quantile(sorted: readonly number[], p: number): number {
  if (sorted.length === 0) {
    throw new Error("quantile: empty input");
  }
  if (sorted.length === 1) return sorted[0];
  const h = (sorted.length - 1) * p;
  const lo = Math.floor(h);
  const hi = Math.ceil(h);
  if (lo === hi) return sorted[lo];
  const frac = h - lo;
  return sorted[lo] + (sorted[hi] - sorted[lo]) * frac;
}

/** Compute a box-plot summary from a list of raw values. Null if fewer
 * than 2 non-null values — callers should render a single dot in that case. */
export function boxSummary(raw: Array<number | null | undefined>): BoxSummary | null {
  const values = raw
    .filter((v): v is number => typeof v === "number" && Number.isFinite(v))
    .slice()
    .sort((a, b) => a - b);
  if (values.length < 2) return null;
  const q1 = quantile(values, 0.25);
  const q3 = quantile(values, 0.75);
  const median = quantile(values, 0.5);
  const iqr = q3 - q1;
  const loCutoff = q1 - 1.5 * iqr;
  const hiCutoff = q3 + 1.5 * iqr;
  const inside = values.filter((v) => v >= loCutoff && v <= hiCutoff);
  const outliers = values.filter((v) => v < loCutoff || v > hiCutoff);
  const whiskerLo = inside.length > 0 ? inside[0] : q1;
  const whiskerHi = inside.length > 0 ? inside[inside.length - 1] : q3;
  return {
    five: {
      min: values[0],
      q1,
      median,
      q3,
      max: values[values.length - 1],
      n: values.length,
    },
    whiskerLo,
    whiskerHi,
    outliers,
    values,
  };
}
