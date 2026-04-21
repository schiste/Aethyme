import { linearScale, padExtent } from "../../lib/scale";

interface Props {
  samples: readonly number[];
  /** Mean of the samples — drawn as a vertical marker. */
  mean: number;
  /** Optional [min, max] to pin the axis; defaults to padded sample range. */
  domain?: readonly [number, number];
  width?: number;
  height?: number;
}

// Tiny horizontal dot strip visualizing the N individual judge samples
// alongside the mean line. A tight cluster with dots piled on the mean
// signals "judge is reliable on this row." A fan-out of dots signals
// "judge was noisy" — the rubric or the answer is ambiguous, and the
// single mean score is hiding that uncertainty.
export default function JudgeDotStrip({
  samples,
  mean,
  domain,
  width = 80,
  height = 16,
}: Props) {
  if (samples.length === 0) {
    return null;
  }

  // Default domain: pad the samples so dots don't sit on the edge. If the
  // samples are all identical we still need a positive-width domain.
  const lo = Math.min(...samples);
  const hi = Math.max(...samples);
  const effectiveDomain = domain ?? padExtent(
    lo === hi ? [lo - 1, hi + 1] : [lo, hi],
    0.15,
  );
  const xScale = linearScale(effectiveDomain, [2, width - 2]);
  const meanX = xScale(mean);
  const midY = height / 2;

  return (
    <svg
      width={width}
      height={height}
      className="block overflow-visible"
      role="img"
      aria-label={`Judge samples: ${samples.map((s) => s.toFixed(1)).join(", ")}; mean ${mean.toFixed(1)}`}
    >
      <line
        x1={2} x2={width - 2}
        y1={midY} y2={midY}
        stroke="var(--color-border)"
        strokeWidth={0.5}
      />
      {samples.map((s, i) => (
        <circle
          key={i}
          cx={xScale(s)}
          cy={midY}
          r={2}
          fill="currentColor"
          fillOpacity={0.6}
        />
      ))}
      <line
        x1={meanX}
        x2={meanX}
        y1={midY - 5}
        y2={midY + 5}
        stroke="currentColor"
        strokeWidth={1.5}
        opacity={0.9}
      />
    </svg>
  );
}
