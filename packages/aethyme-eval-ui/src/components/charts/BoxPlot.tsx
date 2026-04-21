import { useMemo, useState } from "react";
import {
  linearScale,
  bandScale,
  extent,
  padExtent,
} from "../../lib/scale";
import { boxSummary, type BoxSummary } from "../../lib/stats";
import { conditionColor } from "../../lib/chartData";

export interface BoxGroup {
  /** Condition label — used for x-axis tick + color lookup. */
  label: string;
  /** Raw per-run values. null/undefined are filtered out. */
  values: Array<number | null | undefined>;
}

interface Props {
  groups: BoxGroup[];
  yLabel: string;
  formatY?: (v: number) => string;
  width?: number;
  height?: number;
}

const MARGIN = { top: 16, right: 24, bottom: 48, left: 52 };

export default function BoxPlot({
  groups,
  yLabel,
  formatY = (v) => (Number.isInteger(v) ? String(v) : v.toFixed(1)),
  width = 760,
  height = 360,
}: Props) {
  const [hover, setHover] = useState<{
    label: string;
    summary: BoxSummary | null;
    single: number | null;
    x: number;
    y: number;
  } | null>(null);

  const plotW = width - MARGIN.left - MARGIN.right;
  const plotH = height - MARGIN.top - MARGIN.bottom;

  const { xScale, yScale, summaries, singlePoints } = useMemo(() => {
    const summaries = groups.map((g) => ({
      label: g.label,
      summary: boxSummary(g.values),
      single:
        boxSummary(g.values) === null
          ? g.values.find((v): v is number => typeof v === "number" && Number.isFinite(v)) ?? null
          : null,
    }));
    const allValues = groups.flatMap((g) =>
      g.values.filter((v): v is number => typeof v === "number" && Number.isFinite(v)),
    );
    const yExt = extent(allValues);
    const xScale = bandScale(
      summaries.map((s) => s.label),
      [0, plotW],
      0.35,
    );
    const yScale = linearScale(
      yExt ? padExtent(yExt, 0.08) : [0, 100],
      [plotH, 0],
    );
    const singlePoints = summaries
      .filter((s) => s.single !== null)
      .map((s) => ({ label: s.label, value: s.single! }));
    return { xScale, yScale, summaries, singlePoints };
  }, [groups, plotW, plotH]);

  if (summaries.length === 0 || summaries.every((s) => s.summary === null && s.single === null)) {
    return (
      <div className="text-center py-12 text-[var(--color-text-muted)] text-sm">
        No data to plot.
      </div>
    );
  }

  const yTicks = yScale.ticks(6);

  return (
    <div className="relative">
      <svg
        width={width}
        height={height}
        className="block"
        role="img"
        aria-label={`Box plot of ${yLabel} by condition`}
      >
        <g transform={`translate(${MARGIN.left}, ${MARGIN.top})`}>
          {/* Y grid + labels */}
          {yTicks.map((t) => (
            <g key={`yg-${t}`}>
              <line x1={0} x2={plotW} y1={yScale(t)} y2={yScale(t)}
                stroke="var(--color-border)" strokeWidth={0.5} />
              <text
                x={-8} y={yScale(t)} dy="0.32em"
                textAnchor="end" fontSize={10}
                className="fill-[var(--color-text-muted)]"
                fontFamily="ui-monospace, monospace"
              >
                {formatY(t)}
              </text>
            </g>
          ))}

          {/* Y label */}
          <text
            transform={`translate(-36, ${plotH / 2}) rotate(-90)`}
            textAnchor="middle"
            className="fill-[var(--color-text)]"
            fontSize={11}
          >
            {yLabel}
          </text>

          {/* X ticks */}
          {summaries.map(({ label }) => (
            <text
              key={`xt-${label}`}
              x={xScale(label)}
              y={plotH + 18}
              textAnchor="middle"
              fontSize={10}
              className="fill-[var(--color-text-muted)]"
              fontFamily="ui-monospace, monospace"
            >
              {label}
            </text>
          ))}

          {/* Boxes */}
          {summaries.map(({ label, summary }) => {
            if (!summary) return null;
            const cx = xScale(label);
            const bw = xScale.bandwidth;
            const color = conditionColor(label);
            const boxTop = yScale(summary.five.q3);
            const boxBottom = yScale(summary.five.q1);
            const medianY = yScale(summary.five.median);
            const whiskerHi = yScale(summary.whiskerHi);
            const whiskerLo = yScale(summary.whiskerLo);

            return (
              <g
                key={`box-${label}`}
                onMouseEnter={(e) => {
                  const rect = (e.currentTarget.ownerSVGElement as SVGSVGElement)
                    .getBoundingClientRect();
                  setHover({
                    label, summary, single: null,
                    x: rect.left + MARGIN.left + cx,
                    y: rect.top + MARGIN.top + medianY,
                  });
                }}
                onMouseLeave={() => setHover(null)}
                style={{ cursor: "pointer" }}
              >
                {/* Upper whisker */}
                <line
                  x1={cx} x2={cx}
                  y1={whiskerHi} y2={boxTop}
                  stroke={color}
                  strokeWidth={1}
                />
                <line
                  x1={cx - bw * 0.2} x2={cx + bw * 0.2}
                  y1={whiskerHi} y2={whiskerHi}
                  stroke={color}
                  strokeWidth={1}
                />
                {/* Box */}
                <rect
                  x={cx - bw / 2}
                  y={boxTop}
                  width={bw}
                  height={Math.max(1, boxBottom - boxTop)}
                  fill={color}
                  fillOpacity={0.2}
                  stroke={color}
                  strokeWidth={1}
                />
                {/* Median line */}
                <line
                  x1={cx - bw / 2}
                  x2={cx + bw / 2}
                  y1={medianY}
                  y2={medianY}
                  stroke={color}
                  strokeWidth={2}
                />
                {/* Lower whisker */}
                <line
                  x1={cx} x2={cx}
                  y1={boxBottom} y2={whiskerLo}
                  stroke={color}
                  strokeWidth={1}
                />
                <line
                  x1={cx - bw * 0.2} x2={cx + bw * 0.2}
                  y1={whiskerLo} y2={whiskerLo}
                  stroke={color}
                  strokeWidth={1}
                />
                {/* Outliers */}
                {summary.outliers.map((v, idx) => (
                  <circle
                    key={`o-${label}-${idx}`}
                    cx={cx}
                    cy={yScale(v)}
                    r={2.5}
                    fill={color}
                    fillOpacity={0.8}
                  />
                ))}
              </g>
            );
          })}

          {/* Single-value degenerate groups (n<2): just a dot with a hint */}
          {singlePoints.map(({ label, value }) => {
            const cx = xScale(label);
            const cy = yScale(value);
            const color = conditionColor(label);
            return (
              <g key={`single-${label}`}
                onMouseEnter={(e) => {
                  const rect = (e.currentTarget.ownerSVGElement as SVGSVGElement)
                    .getBoundingClientRect();
                  setHover({
                    label, summary: null, single: value,
                    x: rect.left + MARGIN.left + cx,
                    y: rect.top + MARGIN.top + cy,
                  });
                }}
                onMouseLeave={() => setHover(null)}
                style={{ cursor: "pointer" }}
              >
                <circle cx={cx} cy={cy} r={4} fill={color} fillOpacity={0.6} />
                <circle cx={cx} cy={cy} r={8} fill={color} fillOpacity={0.1} />
              </g>
            );
          })}
        </g>
      </svg>

      {hover && (
        <div
          className="fixed pointer-events-none z-50 bg-[#1a1a2e] border border-[var(--color-border)] rounded px-3 py-2 text-xs shadow-xl"
          style={{
            left: Math.min(hover.x + 10, window.innerWidth - 200),
            top: hover.y - 60,
          }}
        >
          <div className="font-mono text-[var(--color-text)]">{hover.label}</div>
          {hover.summary ? (
            <div className="mt-1 space-y-0.5 font-mono text-[10px] text-[var(--color-text-muted)]">
              <div>n = {hover.summary.five.n}</div>
              <div>max: {formatY(hover.summary.five.max)}</div>
              <div>Q3:  {formatY(hover.summary.five.q3)}</div>
              <div>med: {formatY(hover.summary.five.median)}</div>
              <div>Q1:  {formatY(hover.summary.five.q1)}</div>
              <div>min: {formatY(hover.summary.five.min)}</div>
              {hover.summary.outliers.length > 0 && (
                <div>outliers: {hover.summary.outliers.length}</div>
              )}
            </div>
          ) : hover.single !== null ? (
            <div className="mt-1 font-mono text-[10px] text-[var(--color-text-muted)]">
              n = 1 · value {formatY(hover.single)} (insufficient for a box)
            </div>
          ) : null}
        </div>
      )}
    </div>
  );
}
