/**
 * Effectiveness rates are over JUDGED fixes only. The backend sends `null`
 * when nothing in the scope is judged, and every reader here keeps that null:
 * a null rate renders "—" in a table and a gap in a chart, never 0%.
 */
import type { EffectivenessBucket } from "@/types/architecture";

export const NO_RATE = "—";

export type EffectivenessChartPoint = EffectivenessBucket & {
  /** Percentage 0-100, or null when nothing in the bucket is judged. */
  ratePercent: number | null;
};

/** `rate` is a 0-1 fraction or null; null stays null. */
export function toRatePercent(rate: number | null | undefined): number | null {
  return rate == null ? null : rate * 100;
}

export function effectivenessChartData(buckets: EffectivenessBucket[]): EffectivenessChartPoint[] {
  return buckets.map((b) => ({ ...b, ratePercent: toRatePercent(b.effectiveness_rate) }));
}

/** First and last judged rates of a series, for the trend colour. */
export function trendEnds(points: EffectivenessChartPoint[]): {
  first: number | null;
  last: number | null;
} {
  const judged = points.filter((p) => p.ratePercent != null);
  return {
    first: judged[0]?.ratePercent ?? null,
    last: judged[judged.length - 1]?.ratePercent ?? null,
  };
}

/** Percentage text, "—" when there is no rate. */
export function formatRatePercent(ratePercent: number | null | undefined): string {
  return ratePercent == null ? NO_RATE : `${ratePercent.toFixed(1)}%`;
}

/** Colour class for a 0-100 rate; no rate gets the neutral colour. */
export function ratePercentColor(ratePercent: number | null | undefined): string {
  if (ratePercent == null) return "text-zinc-500";
  if (ratePercent >= 80) return "text-green-400";
  if (ratePercent >= 50) return "text-yellow-400";
  return "text-red-400";
}
