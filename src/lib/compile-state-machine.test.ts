/**
 * Tests for compileStateMachineFromSpecs.
 *
 * Focus: the duplicate-elements quarantine path. Successful compilation
 * paths are exercised indirectly by the rest of the runner; these tests
 * pin the behaviour that violates the one-state-per-element invariant
 * returns `{ compiled: false, reason: "quarantined" }` instead of a
 * partial state machine with warnings.
 */

import { describe, it, expect } from "vitest";
import { compileStateMachineFromSpecs } from "./compile-state-machine";
import type { SpecConfig } from "./spec-prompt-builder";

/** Build a minimal SpecConfig with a single state claiming one role+text element. */
function makeSpec(specDescription: string, stateId: string, label: string): SpecConfig {
  return {
    version: "1",
    description: specDescription,
    groups: [],
    stateMachine: {
      states: [
        {
          id: stateId,
          name: stateId,
          elements: [
            {
              role: "button",
              accessibleName: label,
              tagName: "button",
            },
          ],
          transitions: [],
        },
      ],
    },
  };
}

describe("compileStateMachineFromSpecs", () => {
  it("returns compiled=true with no conflicts for disjoint specs", () => {
    const specs: SpecConfig[] = [
      makeSpec("a", "state-a", "Alpha"),
      makeSpec("b", "state-b", "Bravo"),
    ];
    const result = compileStateMachineFromSpecs(specs);
    expect(result.compiled).toBe(true);
    if (result.compiled) {
      expect(result.stats.statesCompiled).toBe(2);
      expect(result.stateMachine.states).toHaveLength(2);
    }
  });

  it("quarantines the compilation when the same element is claimed by two states", () => {
    const specs: SpecConfig[] = [
      // Both states claim a button with accessibleName "Severity" → one
      // element-key, two state IDs → cross-state collision.
      makeSpec("em", "em-filtered", "Severity"),
      makeSpec("findings", "findings-display", "Severity"),
    ];
    const result = compileStateMachineFromSpecs(specs);
    expect(result.compiled).toBe(false);
    if (!result.compiled) {
      expect(result.reason).toBe("quarantined");
      expect(result.quarantine.reason).toBe("duplicate-elements");
      expect(result.quarantine.conflicts).toHaveLength(1);
      const conflict = result.quarantine.conflicts[0];
      expect(conflict.elementKey).toContain("Severity");
      const stateIds = conflict.states.map((s) => s.stateId).sort();
      expect(stateIds).toEqual(["em-filtered", "findings-display"]);
      // Original spec set is preserved so operators can inspect offline.
      expect(result.quarantine.specs).toHaveLength(2);
      // ID is present and non-empty for the filename.
      expect(result.quarantine.id.length).toBeGreaterThan(0);
      expect(result.quarantine.detectedAt).toMatch(/\d{4}-\d{2}-\d{2}T/);
    }
  });

  it("emits exactly one aggregate warning per quarantined compilation", () => {
    const originalWarn = console.warn;
    const calls: string[] = [];
    console.warn = (...args: unknown[]) => {
      calls.push(args.map((a) => (typeof a === "string" ? a : JSON.stringify(a))).join(" "));
    };
    try {
      const specs: SpecConfig[] = [
        makeSpec("a", "sa", "Dup"),
        makeSpec("b", "sb", "Dup"),
        // Third spec adds another duplicate pair — we still expect ONE aggregate
        // warning, not one per duplicate.
        makeSpec("c", "sc", "OtherDup"),
        makeSpec("d", "sd", "OtherDup"),
      ];
      const result = compileStateMachineFromSpecs(specs);
      expect(result.compiled).toBe(false);
      if (!result.compiled) {
        expect(result.quarantine.conflicts).toHaveLength(2);
      }
      const quarantineWarnings = calls.filter((c) =>
        c.includes("[compile-state-machine] quarantined compilation"),
      );
      expect(quarantineWarnings).toHaveLength(1);
    } finally {
      console.warn = originalWarn;
    }
  });

  it("still throws when two observed/ai-fallback states collide (hard invariant)", () => {
    // The stricter enforcement is preserved: observed/ai-fallback collisions
    // are an authoritative-source disagreement and throw synchronously so
    // the bug is surfaced immediately rather than buried in a quarantine.
    const specs: SpecConfig[] = [makeSpec("a", "sa", "Claim"), makeSpec("b", "sb", "Claim")];
    // Force both states to "observed" via a discovery artifact that matches
    // either one by cluster-size proximity + support/contrast thresholds.
    expect(() =>
      compileStateMachineFromSpecs(specs, {
        artifact: {
          states: [
            {
              elements: ["e1"],
              support: 1.0,
              contrast: 1.0,
            },
          ],
        },
      }),
    ).toThrow(/one-state-per-element invariant violated/);
  });
});
