/**
 * Label a cost figure by where it came from.
 *
 * A cost on the Cost Control panel is summed from `phase_token_usage` rows whose
 * figure was either REPORTED by the provider (the Claude CLI's `total_cost_usd`,
 * its list-price figure), ESTIMATED from token counts against a price table, or
 * of UNKNOWN provenance (rows written before the provenance column existed).
 *
 * Either way the figure is a list-price equivalent, never the user's bill: the
 * CLI reports a list-price figure even on subscription auth. Served policy
 * production-and-cost `per-task-cost-is-a-labelled-equivalent` requires the
 * label, reported-over-estimated, and coverage — so every label carries
 * {@link LIST_PRICE_EQUIVALENT} and the provenance shares.
 */

import type { CostProvenance } from "./types";

/** The label every per-task cost figure carries. */
export const LIST_PRICE_EQUIVALENT = "list-price equivalent, not your bill";

/**
 * Whole-percent shares that sum to exactly 100 (largest-remainder rounding;
 * ties go to the earlier part).
 */
function largestRemainderPercents(counts: number[], total: number): number[] {
  const exact = counts.map((n) => (n / total) * 100);
  const floors = exact.map(Math.floor);
  let left = 100 - floors.reduce((a, b) => a + b, 0);
  const order = exact
    .map((x, i) => ({ i, frac: x - Math.floor(x) }))
    .sort((a, b) => b.frac - a.frac || a.i - b.i);
  for (const { i } of order) {
    if (left <= 0) break;
    floors[i] += 1;
    left -= 1;
  }
  return floors;
}

/** A share as shown: never "0%" for a non-empty part, never "100%" beside one. */
function showPercent(pct: number, count: number, total: number): string {
  if (count > 0 && pct < 1) return "<1%";
  if (count < total && pct >= 100) return ">99%";
  return `${pct}%`;
}

/**
 * One short phrase for a figure's provenance, or `null` when there are no rows
 * behind it (nothing to label — not "0% reported").
 *
 * - every row reported  -> "reported"
 * - every row estimated -> "estimated"
 * - every row unknown   -> "provenance unknown"
 * - a mix               -> "of rows: 62% reported, 30% estimated, 8% unknown"
 *   (shares of ROWS, not of dollars; they sum to 100; a non-empty part too
 *   small to round to 1% reads "<1%"; zero parts are omitted)
 */
export function describeCostProvenance(p: CostProvenance | undefined | null): string | null {
  if (!p) return null;
  const counts = [p.reported_rows, p.estimated_rows, p.unknown_rows];
  const total = counts[0] + counts[1] + counts[2];
  if (total <= 0) return null;
  if (p.reported_rows === total) return "reported";
  if (p.estimated_rows === total) return "estimated";
  if (p.unknown_rows === total) return "provenance unknown";
  const pcts = largestRemainderPercents(counts, total);
  const names = ["reported", "estimated", "unknown"];
  const parts: string[] = [];
  counts.forEach((n, i) => {
    if (n > 0) parts.push(`${showPercent(pcts[i], n, total)} ${names[i]}`);
  });
  return `of rows: ${parts.join(", ")}`;
}

/**
 * The full label for a cost figure: always the list-price-equivalent label,
 * followed by its provenance when there are rows behind it.
 */
export function describeCostFigure(p: CostProvenance | undefined | null): string {
  const provenance = describeCostProvenance(p);
  return provenance ? `${LIST_PRICE_EQUIVALENT} · ${provenance}` : LIST_PRICE_EQUIVALENT;
}
