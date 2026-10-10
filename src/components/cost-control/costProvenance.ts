/**
 * Label a cost figure by where it came from.
 *
 * A cost on the Cost Control panel is summed from `phase_token_usage` rows whose
 * figure was either REPORTED by the provider (Claude CLI `total_cost_usd`),
 * ESTIMATED from token counts against a price table, or of UNKNOWN provenance
 * (rows written before the provenance column existed). An estimate must not
 * read as a bill, so the panel says which it is.
 */

import type { CostProvenance } from "./types";

/**
 * One short phrase for a figure's provenance, or `null` when there are no rows
 * behind it (nothing to label — not "0% reported").
 *
 * - every row reported  -> "reported"
 * - every row estimated -> "estimated"
 * - every row unknown   -> "provenance unknown"
 * - a mix               -> "62% reported, 30% estimated, 8% unknown"
 *   (row shares; zero parts are omitted)
 */
export function describeCostProvenance(p: CostProvenance | undefined | null): string | null {
  if (!p) return null;
  const total = p.reported_rows + p.estimated_rows + p.unknown_rows;
  if (total <= 0) return null;
  if (p.reported_rows === total) return "reported";
  if (p.estimated_rows === total) return "estimated";
  if (p.unknown_rows === total) return "provenance unknown";
  const pct = (n: number) => Math.round((n / total) * 100);
  const parts: string[] = [];
  if (p.reported_rows > 0) parts.push(`${pct(p.reported_rows)}% reported`);
  if (p.estimated_rows > 0) parts.push(`${pct(p.estimated_rows)}% estimated`);
  if (p.unknown_rows > 0) parts.push(`${pct(p.unknown_rows)}% unknown`);
  return parts.join(", ");
}
