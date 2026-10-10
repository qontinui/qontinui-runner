import { describe, it, expect } from "vitest";

import { describeCostProvenance } from "./costProvenance";

const p = (reported_rows: number, estimated_rows: number, unknown_rows: number) => ({
  reported_rows,
  estimated_rows,
  unknown_rows,
});

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

  it("gives row shares for a mix, omitting empty parts", () => {
    expect(describeCostProvenance(p(5, 3, 0))).toBe("63% reported, 38% estimated");
    expect(describeCostProvenance(p(1, 1, 2))).toBe("25% reported, 25% estimated, 50% unknown");
  });
});
