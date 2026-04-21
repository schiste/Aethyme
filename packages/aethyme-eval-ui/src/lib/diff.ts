// ---------------------------------------------------------------------------
// Line-based LCS diff for eval outputs.
//
// Eval outputs are long prose, not code — a word-level diff drowns the reader
// in noise. Line-level LCS gives you: "these lines are verbatim matches" vs
// "these two chunks diverged," which is the only question you ask when you're
// comparing explain-repo output across conditions.
//
// The side-by-side viewer uses this to paint aligned "same" lines and marks
// "only in A / only in B" lines without trying to reconcile word-level
// changes. Simple, predictable, good enough.
// ---------------------------------------------------------------------------

export type DiffOp = "same" | "onlyA" | "onlyB";

export interface DiffRow {
  op: DiffOp;
  /** Line index in A (null for onlyB rows). */
  aIndex: number | null;
  /** Line index in B (null for onlyA rows). */
  bIndex: number | null;
  aText: string | null;
  bText: string | null;
}

export interface DiffStats {
  matched: number;
  onlyA: number;
  onlyB: number;
  similarityPct: number;
}

export interface DiffResult {
  rows: DiffRow[];
  stats: DiffStats;
}

/** Compute a line-level LCS diff between two strings.
 *
 * Not optimized for huge inputs — outputs run to a few thousand lines at
 * most in practice, and O(n·m) on that scale is a handful of ms in JS.
 * If this ever gets slow, switch to a patience or histogram diff.
 */
export function diffLines(a: string, b: string): DiffResult {
  const aLines = a.length === 0 ? [] : a.split("\n");
  const bLines = b.length === 0 ? [] : b.split("\n");

  // LCS length table.
  const n = aLines.length;
  const m = bLines.length;
  const dp: number[][] = Array.from({ length: n + 1 }, () =>
    new Array<number>(m + 1).fill(0),
  );
  for (let i = 1; i <= n; i++) {
    for (let j = 1; j <= m; j++) {
      if (aLines[i - 1] === bLines[j - 1]) {
        dp[i][j] = dp[i - 1][j - 1] + 1;
      } else {
        dp[i][j] = Math.max(dp[i - 1][j], dp[i][j - 1]);
      }
    }
  }

  // Walk the table backwards to recover the diff.
  const rows: DiffRow[] = [];
  let i = n, j = m;
  while (i > 0 && j > 0) {
    if (aLines[i - 1] === bLines[j - 1]) {
      rows.unshift({
        op: "same",
        aIndex: i - 1,
        bIndex: j - 1,
        aText: aLines[i - 1],
        bText: bLines[j - 1],
      });
      i--; j--;
    } else if (dp[i - 1][j] >= dp[i][j - 1]) {
      rows.unshift({
        op: "onlyA",
        aIndex: i - 1,
        bIndex: null,
        aText: aLines[i - 1],
        bText: null,
      });
      i--;
    } else {
      rows.unshift({
        op: "onlyB",
        aIndex: null,
        bIndex: j - 1,
        aText: null,
        bText: bLines[j - 1],
      });
      j--;
    }
  }
  while (i > 0) {
    rows.unshift({
      op: "onlyA", aIndex: i - 1, bIndex: null,
      aText: aLines[i - 1], bText: null,
    });
    i--;
  }
  while (j > 0) {
    rows.unshift({
      op: "onlyB", aIndex: null, bIndex: j - 1,
      aText: null, bText: bLines[j - 1],
    });
    j--;
  }

  let matched = 0, onlyA = 0, onlyB = 0;
  for (const row of rows) {
    if (row.op === "same") matched++;
    else if (row.op === "onlyA") onlyA++;
    else onlyB++;
  }
  const denominator = Math.max(n, m, 1);
  const similarityPct = Math.round((matched / denominator) * 1000) / 10;

  return {
    rows,
    stats: { matched, onlyA, onlyB, similarityPct },
  };
}
