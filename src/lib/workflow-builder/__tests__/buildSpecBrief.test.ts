import { describe, it, expect } from "vitest";
import { buildSpecBrief } from "../buildSpecBrief";
import type { SpecConfig } from "../buildSpecWorkflow";

describe("buildSpecBrief", () => {
  it("partitions assertions into deterministic vs semantic and preserves setupHints", () => {
    const specConfig = {
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

    const brief = buildSpecBrief({ specConfig: specConfig as unknown as SpecConfig });

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

  describe("targetStateId resolution", () => {
    it("maps precondition to a matching state by name", () => {
      const specConfig = {
        version: "1",
        description: "test spec",
        groups: [
          {
            id: "g1",
            name: "Findings panel contents",
            description: "Panel checks",
            category: "ui",
            setupActions: [],
            assertions: [
              {
                id: "a1",
                description: "Findings panel shows entries",
                enabled: true,
                precondition: "Findings panel must be open",
                severity: "critical",
                assertionType: "exists",
                target: { criteria: { role: "region" } },
              },
            ],
          },
        ],
        stateMachine: {
          states: [
            {
              id: "findings-panel",
              name: "Findings Panel Open",
              description: "",
              elements: [],
              isInitial: false,
              transitions: [],
            },
            {
              id: "active-terminals",
              name: "Active Terminals",
              description: "",
              elements: [],
              isInitial: true,
              transitions: [],
            },
          ],
        },
      };

      const brief = buildSpecBrief({ specConfig: specConfig as unknown as SpecConfig });
      expect(brief.groups[0].targetStateId).toBe("findings-panel");
    });

    it("resolves via id-token fallback when name does not match", () => {
      const specConfig = {
        version: "1",
        description: "test spec",
        groups: [
          {
            id: "g1",
            name: "Some generic group",
            description: "",
            category: "ui",
            setupActions: [],
            assertions: [
              {
                id: "a1",
                description: "Thing is there",
                enabled: true,
                precondition: "The findings panel must be visible",
                severity: "critical",
                assertionType: "exists",
                target: { criteria: { role: "region" } },
              },
            ],
          },
        ],
        stateMachine: {
          states: [
            {
              id: "findings-panel",
              name: "Totally Different Name",
              description: "",
              elements: [],
              isInitial: false,
              transitions: [],
            },
          ],
        },
      };

      const brief = buildSpecBrief({ specConfig: specConfig as unknown as SpecConfig });
      expect(brief.groups[0].targetStateId).toBe("findings-panel");
    });

    it("leaves targetStateId undefined when no state matches", () => {
      const specConfig = {
        version: "1",
        description: "test spec",
        groups: [
          {
            id: "g1",
            name: "Header banner",
            description: "",
            category: "ui",
            setupActions: [],
            assertions: [
              {
                id: "a1",
                description: "Header exists",
                enabled: true,
                severity: "critical",
                assertionType: "exists",
                target: { criteria: { selector: "#header" } },
              },
            ],
          },
        ],
        stateMachine: {
          states: [
            {
              id: "settings-modal",
              name: "Settings Modal Open",
              description: "",
              elements: [],
              isInitial: false,
              transitions: [],
            },
          ],
        },
      };

      const brief = buildSpecBrief({ specConfig: specConfig as unknown as SpecConfig });
      expect(brief.groups[0].targetStateId).toBeUndefined();
    });

    it("leaves targetStateId undefined when spec has no stateMachine", () => {
      const specConfig = {
        version: "1",
        description: "test spec",
        groups: [
          {
            id: "g1",
            name: "Findings panel contents",
            description: "",
            category: "ui",
            setupActions: [],
            assertions: [
              {
                id: "a1",
                description: "Findings panel shows entries",
                enabled: true,
                precondition: "Findings panel must be open",
                severity: "critical",
                assertionType: "exists",
                target: { criteria: { role: "region" } },
              },
            ],
          },
        ],
        // stateMachine intentionally absent (older specs)
      };

      const brief = buildSpecBrief({ specConfig: specConfig as unknown as SpecConfig });
      expect(brief.groups[0].targetStateId).toBeUndefined();
    });
  });
});
