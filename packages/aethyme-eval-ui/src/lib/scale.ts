// ---------------------------------------------------------------------------
// Minimal scale helpers — enough for scatter/box/heatmap/sparkline.
//
// We deliberately don't pull d3-scale. It's ~30KB + friends and we only
// need a few functions. Hand-rolled keeps them testable and matches the
// "no heavyweight deps" posture of the rest of the repo.
// ---------------------------------------------------------------------------

export interface LinearScale {
  /** Map a data value to a pixel coordinate. */
  (value: number): number;
  /** Input domain — [min, max]. */
  domain: readonly [number, number];
  /** Output range — [pixelStart, pixelEnd]. */
  range: readonly [number, number];
  /** Invert a pixel coordinate back to a data value. */
  invert: (pixel: number) => number;
  /** Pick N "nice" ticks within the domain. */
  ticks: (count?: number) => number[];
}

export function linearScale(
  domain: readonly [number, number],
  range: readonly [number, number],
): LinearScale {
  const [d0, d1] = domain;
  const [r0, r1] = range;
  const dSpan = d1 - d0;
  // Guard against zero-span domains so we don't divide by zero — map every
  // value to the midpoint of the range, which is what every other scale
  // library does too.
  const safeSpan = dSpan === 0 ? 1 : dSpan;

  const scale = ((value: number): number => {
    const t = (value - d0) / safeSpan;
    return r0 + t * (r1 - r0);
  }) as LinearScale;

  scale.domain = domain;
  scale.range = range;
  scale.invert = (pixel: number): number => {
    const t = (pixel - r0) / (r1 - r0);
    return d0 + t * safeSpan;
  };
  scale.ticks = (count: number = 5): number[] => niceTicks(d0, d1, count);

  return scale;
}

/** Produce ~`count` "nice" tick values in [min, max]. */
export function niceTicks(min: number, max: number, count: number = 5): number[] {
  if (min === max) return [min];
  if (count < 2) count = 2;
  const span = max - min;
  const step0 = span / (count - 1);
  const magnitude = Math.pow(10, Math.floor(Math.log10(step0)));
  const normalized = step0 / magnitude;
  // Snap to 1/2/5 × 10^k for readability.
  const snap = normalized < 1.5 ? 1 : normalized < 3 ? 2 : normalized < 7 ? 5 : 10;
  const step = snap * magnitude;
  const first = Math.ceil(min / step) * step;
  const ticks: number[] = [];
  for (let t = first; t <= max + step * 1e-9; t += step) {
    // Round-trip through fixed-decimal to kill FP noise like 0.30000000000000004
    ticks.push(Number(t.toFixed(10)));
  }
  return ticks;
}

/** Band scale for categorical axes (box plots, heatmap columns). */
export interface BandScale {
  /** Map a category to the center of its band in the range. */
  (category: string): number;
  /** Width of each band in the output range. */
  bandwidth: number;
  domain: readonly string[];
  range: readonly [number, number];
}

export function bandScale(
  domain: readonly string[],
  range: readonly [number, number],
  padding: number = 0.2,
): BandScale {
  const [r0, r1] = range;
  const total = r1 - r0;
  const n = Math.max(1, domain.length);
  const step = total / n;
  const innerPadding = step * padding;
  const bandwidth = step - innerPadding;
  const positions = new Map<string, number>();
  domain.forEach((cat, i) => {
    positions.set(cat, r0 + step * i + step / 2);
  });

  const scale = ((category: string): number => {
    return positions.get(category) ?? NaN;
  }) as BandScale;

  scale.bandwidth = bandwidth;
  scale.domain = domain;
  scale.range = range;
  return scale;
}

/** Extent of numeric values — null-safe, returns null if no finite values. */
export function extent(values: Array<number | null | undefined>): [number, number] | null {
  let lo = Infinity;
  let hi = -Infinity;
  for (const v of values) {
    if (typeof v !== "number" || !Number.isFinite(v)) continue;
    if (v < lo) lo = v;
    if (v > hi) hi = v;
  }
  if (lo === Infinity) return null;
  return [lo, hi];
}

/** Expand a [min, max] pair by a fraction on each end so plotted points
 * don't butt against the axis. */
export function padExtent(
  ext: readonly [number, number],
  fraction: number = 0.05,
): [number, number] {
  const [a, b] = ext;
  const span = b - a;
  if (span === 0) {
    const pad = Math.abs(a) * fraction || 1;
    return [a - pad, b + pad];
  }
  return [a - span * fraction, b + span * fraction];
}
