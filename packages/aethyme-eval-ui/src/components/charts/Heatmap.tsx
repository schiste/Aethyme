import { bandScale } from "../../lib/scale";

export interface HeatmapCell {
  row: string;
  column: string;
  /** Numeric value — drives the color. null renders as an empty/greyed cell. */
  value: number | null;
  /** N contributing runs — surfaced in tooltip. */
  n?: number;
  /** Optional secondary label drawn below the value (e.g. delta). */
  subLabel?: string;
}

interface Props {
  rows: readonly string[];
  columns: readonly string[];
  cells: HeatmapCell[];
  /** [min, max] for the color scale. Null → compute from cells. */
  domain?: readonly [number, number];
  /** true when higher values are better — flips the color ramp. */
  higherIsBetter?: boolean;
  /** Optional pre-formatter for cell values. */
  formatValue?: (v: number) => string;
  width?: number;
  rowHeight?: number;
}

function interpolateColor(
  t: number, // 0..1
  higherIsBetter: boolean,
): string {
  // Diverging palette anchored at 0.5 (neutral slate).
  // At t=0: red if higher-is-better, else green
  // At t=1: green if higher-is-better, else red
  const clamped = Math.max(0, Math.min(1, t));
  const good = [16, 185, 129];  // emerald-500
  const bad  = [239, 68, 68];   // red-500
  const neutral = [75, 85, 99]; // gray-600
  const low = higherIsBetter ? bad : good;
  const hi  = higherIsBetter ? good : bad;
  const mix = clamped < 0.5
    ? interp(low, neutral, clamped * 2)
    : interp(neutral, hi, (clamped - 0.5) * 2);
  return `rgb(${mix[0]}, ${mix[1]}, ${mix[2]})`;
}

function interp(a: number[], b: number[], t: number): number[] {
  return [0, 1, 2].map((i) => Math.round(a[i] + (b[i] - a[i]) * t));
}

export default function Heatmap({
  rows,
  columns,
  cells,
  domain,
  higherIsBetter = true,
  formatValue = (v) => (Number.isInteger(v) ? String(v) : v.toFixed(1)),
  width = 760,
  rowHeight = 56,
}: Props) {
  const margin = { top: 48, right: 24, bottom: 16, left: 120 };
  const plotW = width - margin.left - margin.right;
  const plotH = rows.length * rowHeight;
  const height = plotH + margin.top + margin.bottom;

  const xScale = bandScale(columns, [0, plotW], 0.08);
  const yScale = bandScale(rows, [0, plotH], 0.08);

  // Build color domain from the cells if caller didn't provide one.
  let minVal = Infinity;
  let maxVal = -Infinity;
  for (const c of cells) {
    if (c.value === null) continue;
    if (c.value < minVal) minVal = c.value;
    if (c.value > maxVal) maxVal = c.value;
  }
  const effectiveDomain = domain ?? (
    minVal === Infinity ? [0, 1] as const : [minVal, maxVal] as const
  );
  const [dMin, dMax] = effectiveDomain;
  const dSpan = dMax - dMin || 1;

  // Cells keyed by `row|column`
  const cellMap = new Map<string, HeatmapCell>();
  for (const c of cells) cellMap.set(`${c.row}|${c.column}`, c);

  return (
    <div className="overflow-x-auto">
      <svg
        width={width}
        height={height}
        className="block"
        role="img"
        aria-label="Heatmap of metric by row × column"
      >
        {/* Column headers */}
        {columns.map((col) => (
          <text
            key={`colhdr-${col}`}
            x={margin.left + xScale(col)}
            y={margin.top - 12}
            textAnchor="middle"
            fontSize={11}
            fontFamily="ui-monospace, monospace"
            className="fill-[var(--color-text-muted)]"
          >
            {col}
          </text>
        ))}

        {/* Row labels */}
        {rows.map((r) => (
          <text
            key={`rowlbl-${r}`}
            x={margin.left - 10}
            y={margin.top + yScale(r)}
            dy="0.32em"
            textAnchor="end"
            fontSize={11}
            fontFamily="ui-monospace, monospace"
            className="fill-[var(--color-text-muted)]"
          >
            {r}
          </text>
        ))}

        {/* Cells */}
        {rows.map((r) =>
          columns.map((col) => {
            const key = `${r}|${col}`;
            const cell = cellMap.get(key);
            const cx = margin.left + xScale(col) - xScale.bandwidth / 2;
            const cy = margin.top + yScale(r) - yScale.bandwidth / 2;
            const w = xScale.bandwidth;
            const h = yScale.bandwidth;
            if (!cell || cell.value === null) {
              return (
                <g key={key}>
                  <rect
                    x={cx} y={cy} width={w} height={h}
                    fill="var(--color-border)"
                    fillOpacity={0.2}
                    stroke="var(--color-border)"
                    strokeWidth={0.5}
                  />
                  <text
                    x={cx + w / 2}
                    y={cy + h / 2}
                    dy="0.32em"
                    textAnchor="middle"
                    fontSize={11}
                    className="fill-[var(--color-text-muted)]"
                    fontFamily="ui-monospace, monospace"
                  >
                    —
                  </text>
                </g>
              );
            }
            const t = (cell.value - dMin) / dSpan;
            const fill = interpolateColor(t, higherIsBetter);
            const tip = [
              `${r} × ${col}`,
              `value: ${formatValue(cell.value)}`,
              typeof cell.n === "number" ? `n: ${cell.n}` : null,
              cell.subLabel ?? null,
            ].filter(Boolean).join("\n");
            // White text on dark cells, light-grey on paler ones. Rough proxy
            // for luminance: use white when t is far from 0.5 (bright color).
            const textLight =
              Math.abs(t - 0.5) > 0.18 ? "#ffffff" : "rgba(255,255,255,0.82)";
            return (
              <g key={key}>
                <title>{tip}</title>
                <rect
                  x={cx} y={cy} width={w} height={h}
                  fill={fill}
                  fillOpacity={0.85}
                />
                <text
                  x={cx + w / 2}
                  y={cy + h / 2 - (cell.subLabel ? 7 : 0)}
                  dy="0.32em"
                  textAnchor="middle"
                  fontSize={13}
                  fontWeight={600}
                  fontFamily="ui-monospace, monospace"
                  fill={textLight}
                >
                  {formatValue(cell.value)}
                </text>
                {cell.subLabel && (
                  <text
                    x={cx + w / 2}
                    y={cy + h / 2 + 10}
                    dy="0.32em"
                    textAnchor="middle"
                    fontSize={10}
                    fontFamily="ui-monospace, monospace"
                    fill={textLight}
                    opacity={0.85}
                  >
                    {cell.subLabel}
                  </text>
                )}
              </g>
            );
          }),
        )}
      </svg>

      {/* Legend strip */}
      <div className="mt-3 flex items-center gap-3 text-xs text-[var(--color-text-muted)]">
        <span>{formatValue(dMin)}</span>
        <div
          className="flex-1 h-2 rounded"
          style={{
            background: `linear-gradient(to right,
              ${interpolateColor(0, higherIsBetter)},
              ${interpolateColor(0.5, higherIsBetter)},
              ${interpolateColor(1, higherIsBetter)})`,
          }}
        />
        <span>{formatValue(dMax)}</span>
      </div>
    </div>
  );
}
