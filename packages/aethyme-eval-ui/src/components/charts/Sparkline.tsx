import { linearScale, extent, padExtent } from "../../lib/scale";

export interface SparklinePoint {
  date: string; // ISO
  value: number;
  label?: string;
}

interface Props {
  points: SparklinePoint[];
  width?: number;
  height?: number;
  /** Color of the line + dots. */
  color?: string;
  /** Format a value for the tooltip. */
  formatValue?: (v: number) => string;
  /** Optional target line (e.g. control baseline) rendered as a dashed
   * horizontal reference. */
  target?: number;
}

// Compact inline sparkline. No axes — just the shape. The value at each
// vertex is exposed via a native <title> tooltip so a hover gives the
// timestamp + value without us building a custom popover (this chart is
// meant to live 10-wide in a grid).
export default function Sparkline({
  points,
  width = 140,
  height = 32,
  color = "currentColor",
  formatValue = (v) => v.toFixed(1),
  target,
}: Props) {
  if (points.length === 0) {
    return (
      <svg width={width} height={height} className="block">
        <line
          x1={0} x2={width}
          y1={height / 2} y2={height / 2}
          stroke="var(--color-border)"
          strokeWidth={0.5}
          strokeDasharray="2 2"
        />
      </svg>
    );
  }

  // Sort chronologically so the line draws left→right.
  const ordered = [...points].sort((a, b) => a.date.localeCompare(b.date));
  const xRaw = ordered.map((_, i) => i);
  const xScale = linearScale([0, Math.max(1, xRaw.length - 1)], [2, width - 2]);
  const yExt = extent(ordered.map((p) => p.value)) ?? [0, 1];
  // Include target in the y-domain so the reference line isn't clipped.
  const effectiveYExt = typeof target === "number"
    ? [Math.min(yExt[0], target), Math.max(yExt[1], target)] as const
    : yExt;
  const yScale = linearScale(padExtent(effectiveYExt, 0.10), [height - 2, 2]);

  const path = ordered
    .map((p, i) => {
      const cmd = i === 0 ? "M" : "L";
      return `${cmd}${xScale(i).toFixed(1)},${yScale(p.value).toFixed(1)}`;
    })
    .join(" ");

  const latest = ordered[ordered.length - 1];
  const earliest = ordered[0];
  const delta = latest.value - earliest.value;
  const trendColor =
    Math.abs(delta) < 0.01 * Math.abs(latest.value || 1)
      ? "var(--color-text-muted)"
      : delta > 0
        ? "var(--color-score-green)"
        : "var(--color-score-red)";

  return (
    <svg
      width={width}
      height={height}
      className="block"
      role="img"
      aria-label={`Trend of ${ordered.length} points`}
    >
      {/* Reference target line */}
      {typeof target === "number" && (
        <line
          x1={2} x2={width - 2}
          y1={yScale(target)} y2={yScale(target)}
          stroke="var(--color-text-muted)"
          strokeWidth={0.5}
          strokeDasharray="3 3"
        />
      )}
      {/* The line */}
      <path
        d={path}
        fill="none"
        stroke={color}
        strokeWidth={1.5}
        strokeLinecap="round"
        strokeLinejoin="round"
        opacity={0.85}
      />
      {/* Dots at each point (with native tooltip). */}
      {ordered.map((p, i) => {
        const cx = xScale(i);
        const cy = yScale(p.value);
        const isLatest = i === ordered.length - 1;
        return (
          <g key={`${p.date}-${i}`}>
            <title>
              {`${p.date.slice(0, 10)}: ${formatValue(p.value)}${p.label ? ` · ${p.label}` : ""}`}
            </title>
            <circle
              cx={cx} cy={cy}
              r={isLatest ? 2.4 : 1.6}
              fill={isLatest ? trendColor : color}
              stroke={isLatest ? trendColor : "none"}
              strokeWidth={isLatest ? 1 : 0}
              fillOpacity={isLatest ? 1 : 0.75}
            />
          </g>
        );
      })}
    </svg>
  );
}
