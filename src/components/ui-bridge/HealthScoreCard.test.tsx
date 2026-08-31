/**
 * The card's half of "an unknown health score must not render as a number".
 *
 * `GET /ui-bridge/analytics/health-score` now returns `overall_score: null`
 * (and null rates) whenever the underlying denominators are zero. This card is
 * the PROJECTION of that typed unknown, so the defect can recur here even with
 * the API correct: `Math.round(null * 100)` is `0` in JavaScript, and
 * `scoreColor(0)` is red while `scoreColor(0.7)` is yellow — any of those is a
 * verdict the data does not support.
 *
 * `environment: "node"` vitest, so these pin the exported predicates rather
 * than rendering (same precedent as `SessionCountBanner.test.tsx`). The
 * rendered branch itself is verified live against a running runner.
 */

import { describe, it, expect } from "vitest";

import { hasScore, formatMetricValue, unknownInputLabels } from "./HealthScoreCard";

describe("hasScore", () => {
  it("is false for a null score, so nothing colours or labels it", () => {
    expect(hasScore(null)).toBe(false);
  });

  it("is false for a body with no such key at all", () => {
    expect(hasScore(undefined)).toBe(false);
  });

  it("is true for a real score, including the extremes", () => {
    expect(hasScore(0)).toBe(true);
    expect(hasScore(0.735)).toBe(true);
    expect(hasScore(1)).toBe(true);
  });

  it("rejects a non-finite number rather than colouring it", () => {
    expect(hasScore(Number.NaN)).toBe(false);
    expect(hasScore(Number.POSITIVE_INFINITY)).toBe(false);
  });
});

describe("formatMetricValue", () => {
  it("renders a null rate as Unknown, never as 0%", () => {
    expect(formatMetricValue(null, "percent")).toBe("Unknown");
    expect(formatMetricValue(undefined, "percent")).toBe("Unknown");
  });

  it("keeps a MEASURED zero as 0% — that one is a real measurement", () => {
    expect(formatMetricValue(0, "percent")).toBe("0%");
  });

  it("renders measured rates as rounded percentages", () => {
    expect(formatMetricValue(0.7, "percent")).toBe("70%");
    expect(formatMetricValue(0.2, "percent")).toBe("20%");
    expect(formatMetricValue(1, "percent")).toBe("100%");
  });

  it("renders counts verbatim, including a real stall count", () => {
    // The zero-interactions-with-stalls case as the card sees it: the rate is
    // Unknown while the count beside it is the true 3.
    expect(formatMetricValue(3, "count")).toBe("3");
    expect(formatMetricValue(0, "count")).toBe("0");
  });
});

describe("unknownInputLabels", () => {
  it("names the missing inputs in human terms", () => {
    expect(
      unknownInputLabels([
        "element_success_rate",
        "regression_rate",
        "stall_frequency",
        "overall_score",
      ]),
    ).toEqual(["Element Success Rate", "Regression Rate", "Stall Frequency"]);
  });

  it("drops overall_score — the card is already saying the score is unknown", () => {
    expect(unknownInputLabels(["overall_score"])).toEqual([]);
  });

  it("is empty when everything was measured", () => {
    expect(unknownInputLabels([])).toEqual([]);
    expect(unknownInputLabels(undefined)).toEqual([]);
  });

  it("passes an unrecognised field through rather than dropping it", () => {
    expect(unknownInputLabels(["some_future_rate"])).toEqual(["some_future_rate"]);
  });
});
