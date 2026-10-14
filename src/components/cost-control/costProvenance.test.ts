import { describe, it, expect } from "vitest";

import {
  LIST_PRICE_EQUIVALENT,
  describeCostFigure,
  describeCostProvenance,
} from "./costProvenance";

const p = (reported_rows: number, estimated_rows: number, unknown_rows: number) => ({
  reported_rows,
  estimated_rows,
  unknown_rows,
});

/** The whole-percent numbers in a label, "<1%" and ">99%" excluded. */
const percents = (label: string | null) =>
  [...(label ?? "").matchAll(/(?<![<>])(\d+)%/g)].map((m) => Number(m[1]));

describe("describeCostProvenance", () => {
  it("labels nothing when there are no rows", () => {
    expect(describeCostProvenance(p(0, 0, 0))).toBeNull();
    expect(describeCostProvenance(undefined)).toBeNull();
  });

  it("names a single source plainly", () => {
    expect(describeCostProvenance(p(3, 0, 0))).toBe("reported");
    expect(describeCostProvenance(p(0, 5, 0))).toBe("estimated");
    expect(describeCostProvenance(p(0, 0, 2))).toBe("provenance unknown");
  });

  it("gives row shares for a mix, says they are of rows, and omits empty parts", () => {
    expect(describeCostProvenance(p(5, 3, 0))).toBe("of rows: 63% reported, 37% estimated");
    expect(describeCostProvenance(p(1, 1, 2))).toBe(
      "of rows: 25% reported, 25% estimated, 50% unknown",
    );
  });

  it("shares always sum to 100 (largest remainder, not independent rounding)", () => {
    for (const [r, e, u] of [
      [5, 3, 0],
      [1, 1, 1],
      [2, 2, 2],
      [1, 2, 4],
      [7, 11, 13],
    ]) {
      expect(percents(describeCostProvenance(p(r, e, u))).reduce((a, b) => a + b, 0)).toBe(100);
    }
  });

  it("never shows a non-empty minority as 0% or the majority beside it as 100%", () => {
    expect(describeCostProvenance(p(999, 1, 0))).toBe("of rows: >99% reported, <1% estimated");
    expect(describeCostProvenance(p(1, 300, 0))).toBe("of rows: <1% reported, >99% estimated");
  });
});

describe("describeCostFigure", () => {
  it("always says the figure is a list-price equivalent, not the bill", () => {
    expect(LIST_PRICE_EQUIVALENT).toBe("list-price equivalent, not your bill");
    expect(describeCostFigure(undefined)).toBe(LIST_PRICE_EQUIVALENT);
    expect(describeCostFigure(p(0, 0, 0))).toBe(LIST_PRICE_EQUIVALENT);
    expect(describeCostFigure(p(3, 0, 0))).toBe(`${LIST_PRICE_EQUIVALENT} · reported`);
    expect(describeCostFigure(p(5, 3, 0))).toBe(
      `${LIST_PRICE_EQUIVALENT} · of rows: 63% reported, 37% estimated`,
    );
  });
});
