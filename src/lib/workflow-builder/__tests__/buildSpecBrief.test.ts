import { describe, it, expect } from "vitest";
import { buildSpecBrief } from "../buildSpecBrief";
import type { SpecConfig } from "../buildSpecWorkflow";

describe("buildSpecBrief", () => {
  it("partitions assertions into deterministic vs semantic and preserves setupHints", () => {
    const specConfig: SpecConfig = {
      version: "1",
      description: "test spec",
      groups: [
        {
          id: "g1",
          name: "Findings panel",
          description: "Panel checks",
          category: "ui",
          setupActions: [
            {
              type: "click",
              target: { criteria: { selector: "#open-panel" } },
            },
          ],
          assertions: [
            {
              id: "a1",
              description: "Results header exists",
              severity: "critical",
              enabled: true,
              assertionType: "exists",
              target: { criteria: { selector: "#results-header" } },
            },
            {
              id: "a2",
              description: "Results look coherent to a human",
              severity: "minor",
              enabled: true,
              assertionType: "semantic",
            },
          ],
        },
      ],
    };

    const brief = buildSpecBrief({ specConfig });

    expect(brief.version).toBe("1");
    expect(brief.elementSource).toBe("control");
    expect(brief.groups).toHaveLength(1);

    const [group] = brief.groups;
    expect(group.deterministicAssertions).toHaveLength(1);
    expect(group.deterministicAssertions[0].id).toBe("a1");
    expect(group.semanticAssertions).toHaveLength(1);
    expect(group.semanticAssertions[0].id).toBe("a2");

    // setupHints copied verbatim from group.setupActions
    expect(group.setupHints).toHaveLength(1);
    expect(group.setupHints[0].type).toBe("click");
    expect(group.setupHints[0].target?.criteria).toEqual({ selector: "#open-panel" });

    // Heuristic precondition picked up from "panel" in the group name.
    expect(group.preconditions).toContain("Panel must be open");

    // Summary mentions group and assertion counts.
    expect(brief.summary).toContain("1 groups");
    expect(brief.summary).toContain("2 assertions");
  });
});
