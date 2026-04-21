import { useMemo, useState } from "react";
import {
  linearScale,
  extent,
  padExtent,
} from "../../lib/scale";
import { conditionColor, type ParetoPoint } from "../../lib/chartData";

interface Props {
  points: ParetoPoint[];
  xField: "cost" | "totalTokens" | "durationSeconds";
  xLabel: string;
  /** Format a raw x value for axis/tooltip. */
  formatX: (v: number) => string;
  yField: "qualityScore" | "judgeScore";
  yLabel: string;
  width?: number;
  height?: number;
}

const MARGIN = { top: 16, right: 24, bottom: 44, left: 52 };

export default function Scatter({
  points,
  xField,
  xLabel,
  formatX,
  yField,
  yLabel,
  width = 760,
  height = 420,
}: Props) {
  const [hover, setHover] = useState<ParetoPoint | null>(null);

  const plotW = width - MARGIN.left - MARGIN.right;
  const plotH = height - MARGIN.top - MARGIN.bottom;

  const { xScale, yScale, plotted } = useMemo(() => {
    const plotted = points.filter(
      (p) => typeof p[xField] === "number" && typeof p[yField] === "number",
    );
    const xExt = extent(plotted.map((p) => p[xField] as number));
    const yExt = extent(plotted.map((p) => p[yField] as number));
    const xScale = linearScale(
      xExt ? padExtent(xExt, 0.08) : [0, 1],
      [0, plotW],
    );
    const yScale = linearScale(
      yExt ? padExtent(yExt, 0.08) : [0, 100],
      [plotH, 0], // inverted: SVG y=0 is top
    );
    return { xScale, yScale, plotted };
  }, [points, xField, yField, plotW, plotH]);

  if (plotted.length === 0) {
    return (
      <div className="text-center py-12 text-[var(--color-text-muted)] text-sm">
        No points have both {xLabel} and {yLabel}.
      </div>
    );
  }

  const xTicks = xScale.ticks(6);
  const yTicks = yScale.ticks(6);

  // Unique conditions for the legend — order by first appearance so the
  // legend matches scan order.
  const seenConditions: string[] = [];
  const conditionSet = new Set<string>();
  for (const p of plotted) {
    if (!conditionSet.has(p.condition)) {
      conditionSet.add(p.condition);
      seenConditions.push(p.condition);
    }
  }

  return (
    <div className="relative">
      <svg
        width={width}
        height={height}
        className="block"
        role="img"
        aria-label={`Scatter plot of ${yLabel} vs ${xLabel}`}
      >
        <g transform={`translate(${MARGIN.left}, ${MARGIN.top})`}>
          {/* Grid + axes */}
          {yTicks.map((t) => (
            <g key={`yg-${t}`}>
              <line
                x1={0} x2={plotW}
                y1={yScale(t)} y2={yScale(t)}
                stroke="var(--color-border)"
                strokeWidth={0.5}
              />
              <text
                x={-8}
                y={yScale(t)}
                dy="0.32em"
                textAnchor="end"
                className="fill-[var(--color-text-muted)]"
                fontSize={10}
                fontFamily="ui-monospace, monospace"
              >
                {Number.isInteger(t) ? t : t.toFixed(1)}
              </text>
            </g>
          ))}
          {xTicks.map((t) => (
            <g key={`xg-${t}`}>
              <line
                x1={xScale(t)} x2={xScale(t)}
                y1={0} y2={plotH}
                stroke="var(--color-border)"
                strokeWidth={0.5}
              />
              <text
                x={xScale(t)}
                y={plotH + 16}
                textAnchor="middle"
                className="fill-[var(--color-text-muted)]"
                fontSize={10}
                fontFamily="ui-monospace, monospace"
              >
                {formatX(t)}
              </text>
            </g>
          ))}

          {/* Axis labels */}
          <text
            x={plotW / 2}
            y={plotH + 36}
            textAnchor="middle"
            className="fill-[var(--color-text)]"
            fontSize={11}
          >
            {xLabel}
          </text>
          <text
            transform={`translate(-36, ${plotH / 2}) rotate(-90)`}
            textAnchor="middle"
            className="fill-[var(--color-text)]"
            fontSize={11}
          >
            {yLabel}
          </text>

          {/* Points + stdev bars */}
          {plotted.map((p) => {
            const cx = xScale(p[xField] as number);
            const cy = yScale(p[yField] as number);
            const stdev = yField === "qualityScore" ? p.qualityStdev : null;
            const color = conditionColor(p.condition);
            const failed = p.deliverableStatus === "failed";
            return (
              <g
                key={p.id}
                onMouseEnter={() => setHover(p)}
                onMouseLeave={() => setHover(null)}
                style={{ cursor: "pointer" }}
              >
                {typeof stdev === "number" && (
                  <line
                    x1={cx} x2={cx}
                    y1={yScale((p[yField] as number) - stdev)}
                    y2={yScale((p[yField] as number) + stdev)}
                    stroke={color}
                    strokeWidth={1}
                    opacity={0.5}
                  />
                )}
                <circle
                  cx={cx}
                  cy={cy}
                  r={5 + Math.min(3, p.n - 1)}
                  fill={color}
                  fillOpacity={failed ? 0.25 : 0.75}
                  stroke={color}
                  strokeWidth={failed ? 1.5 : 0}
                  strokeDasharray={failed ? "2 2" : undefined}
                />
              </g>
            );
          })}
        </g>
      </svg>

      {/* Legend */}
      <div className="flex flex-wrap gap-3 mt-2 text-xs">
        {seenConditions.map((cond) => (
          <span key={cond} className="inline-flex items-center gap-1.5 text-[var(--color-text-muted)]">
            <span
              className="inline-block w-2.5 h-2.5 rounded-full"
              style={{ background: conditionColor(cond) }}
            />
            <span className="font-mono">{cond}</span>
          </span>
        ))}
        <span className="inline-flex items-center gap-1.5 text-[var(--color-text-muted)] ml-2">
          <span className="inline-block w-2.5 h-2.5 rounded-full border border-current border-dashed" />
          <span>failed deliverable</span>
        </span>
      </div>

      {/* Tooltip */}
      {hover && (
        <div
          className="absolute pointer-events-none z-10 bg-[#1a1a2e] border border-[var(--color-border)] rounded px-3 py-2 text-xs shadow-xl max-w-xs"
          style={{
            left: Math.min(
              MARGIN.left + xScale(hover[xField] as number) + 10,
              width - 180,
            ),
            top: MARGIN.top + yScale(hover[yField] as number) - 10,
          }}
        >
          <div className="font-mono text-[var(--color-text)]">
            {hover.evalType} · {hover.target} · {hover.model}
          </div>
          <div className="font-mono text-[var(--color-text-muted)]">
            {hover.condition} · {hover.reasoning}
            {hover.scenario && ` · ${hover.scenario}`}
          </div>
          <div className="mt-1 space-y-0.5 text-[var(--color-text)]">
            <div>
              {yLabel}: <span className="font-mono">
                {(hover[yField] as number).toFixed(1)}
                {hover.qualityStdev !== null && yField === "qualityScore" &&
                  ` ± ${hover.qualityStdev.toFixed(1)}`}
              </span>
            </div>
            <div>
              {xLabel}: <span className="font-mono">{formatX(hover[xField] as number)}</span>
            </div>
            <div className="text-[var(--color-text-muted)]">
              n = {hover.n}{hover.deliverableStatus && ` · deliverable ${hover.deliverableStatus}`}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
