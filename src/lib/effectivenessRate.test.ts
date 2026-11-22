import { describe, expect, it } from "vitest";
import type { EffectivenessBucket } from "@/types/architecture";
import {
  NO_RATE,
  effectivenessChartData,
  formatRatePercent,
  ratePercentColor,
  toRatePercent,
  trendEnds,
} from "./effectivenessRate";

const bucket = (name: string, rate: number | null): EffectivenessBucket => ({
  bucket: name,
  total: 4,
  effective: 1,
  ineffective: 1,
  regression: 0,
  inconclusive: 0,
  unjudged: 2,
  effectiveness_rate: rate,
});

describe("effectiveness rate mappers keep an unjudged rate null", () => {
  it("maps a fraction to a percentage and null to null", () => {
    expect(toRatePercent(0.25)).toBe(25);
    expect(toRatePercent(0)).toBe(0);
    expect(toRatePercent(null)).toBeNull();
  });

  it("a bucket with nothing judged charts as a gap, not 0%", () => {
    const data = effectivenessChartData([bucket("w1", 0.5), bucket("w2", null)]);
    expect(data.map((d) => d.ratePercent)).toEqual([50, null]);
  });

  it("trend ends skip unjudged buckets", () => {
    const data = effectivenessChartData([bucket("a", null), bucket("b", 0.2), bucket("c", null)]);
    expect(trendEnds(data)).toEqual({ first: 20, last: 20 });
    expect(trendEnds(effectivenessChartData([bucket("a", null)]))).toEqual({
      first: null,
      last: null,
    });
  });
});

describe("rate cell", () => {
  it("renders an em dash for no rate and a percentage otherwise", () => {
    expect(NO_RATE).toBe("—");
    expect(formatRatePercent(null)).toBe("—");
    expect(formatRatePercent(undefined)).toBe("—");
    expect(formatRatePercent(0)).toBe("0.0%");
    expect(formatRatePercent(66.666)).toBe("66.7%");
  });

  it("colours no rate neutrally, never as a failing 0%", () => {
    expect(ratePercentColor(null)).toBe("text-zinc-500");
    expect(ratePercentColor(0)).toBe("text-red-400");
    expect(ratePercentColor(60)).toBe("text-yellow-400");
    expect(ratePercentColor(90)).toBe("text-green-400");
  });
});
