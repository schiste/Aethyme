// Stacked horizontal bar showing the proportion of tool calls by tool name.
// Clicking shows a breakdown; otherwise this is passive viz that surfaces
// the qualitative "what tools did the agent actually use" question that
// a single count-of-tools number can't answer.

export interface ToolCount {
  name: string;
  count: number;
}

interface Props {
  tools: readonly ToolCount[];
  width?: number;
  height?: number;
}

// A stable palette so the same tool gets the same color across rows.
// Hashing the tool name into the palette makes the mapping deterministic
// without requiring a global color registry.
const PALETTE = [
  "#3b82f6", // blue
  "#10b981", // emerald
  "#a855f7", // purple
  "#f59e0b", // amber
  "#ef4444", // red
  "#06b6d4", // cyan
  "#f97316", // orange
  "#ec4899", // pink
  "#14b8a6", // teal
  "#eab308", // yellow
  "#6366f1", // indigo
];

function hashToIndex(s: string, mod: number): number {
  let h = 0;
  for (let i = 0; i < s.length; i++) {
    h = (h * 31 + s.charCodeAt(i)) | 0;
  }
  return Math.abs(h) % mod;
}

export function toolColor(name: string): string {
  return PALETTE[hashToIndex(name, PALETTE.length)];
}

export default function ToolMixBar({ tools, width = 120, height = 8 }: Props) {
  if (tools.length === 0) return null;
  const total = tools.reduce((s, t) => s + t.count, 0);
  if (total === 0) return null;

  // Draw segments left to right in descending count order — the visual
  // weight of the most-used tool lands at the left edge where the eye
  // starts scanning.
  const ordered = [...tools].sort((a, b) => b.count - a.count);
  let offset = 0;
  const segments = ordered.map((t) => {
    const w = (t.count / total) * width;
    const seg = { ...t, x: offset, w };
    offset += w;
    return seg;
  });

  const label = segments
    .map((s) => `${s.name} ${Math.round((s.count / total) * 100)}%`)
    .join(" · ");

  return (
    <svg
      width={width}
      height={height}
      className="block rounded overflow-hidden"
      role="img"
      aria-label={`Tool mix: ${label}`}
    >
      <title>{segments.map((s) => `${s.name}: ${s.count}`).join("\n")}</title>
      {segments.map((s) => (
        <rect
          key={s.name}
          x={s.x}
          y={0}
          width={s.w}
          height={height}
          fill={toolColor(s.name)}
        />
      ))}
    </svg>
  );
}
