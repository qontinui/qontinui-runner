/**
 * Tests for compileStateMachineFromSpecs.
 *
 * Focus: the duplicate-elements quarantine path. Successful compilation
 * paths are exercised indirectly by the rest of the runner; these tests
 * pin two invariants:
 *   1. Element uniqueness is enforced **per-spec**: two states inside the
 *      same spec sharing an element triggers a quarantine.
 *   2. The same element appearing in two different specs is FINE — that's
 *      the whole point of per-spec scoping (e.g. "Save" button on
 *      TasksPage and ChecksPage shouldn't collide).
 */

import { describe, it, expect } from "vitest";
import { compileStateMachineFromSpecs } from "./compile-state-machine";
import type { SpecConfig, SpecState } from "./spec-prompt-builder";

/**
 * Build a minimal SpecConfig with one or more states. Each state claims a
 * single button element with the given accessible-name label.
 */
function makeSpec(
  specDescription: string,
  states: Array<{ stateId: string; label: string }>,
): SpecConfig {
  const specStates: SpecState[] = states.map(({ stateId, label }) => ({
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
  }));
  return {
    version: "1",
    description: specDescription,
    groups: [],
    stateMachine: {
      states: specStates,
    },
  };
}

/** Convenience for the common single-state case. */
function makeSingleStateSpec(specDescription: string, stateId: string, label: string): SpecConfig {
  return makeSpec(specDescription, [{ stateId, label }]);
}

describe("compileStateMachineFromSpecs", () => {
  it("returns compiled=true with no conflicts for disjoint specs", () => {
    const inputs = [
      { specId: "a", config: makeSingleStateSpec("a", "state-a", "Alpha") },
      { specId: "b", config: makeSingleStateSpec("b", "state-b", "Bravo") },
    ];
    const result = compileStateMachineFromSpecs(inputs);
    expect(result.compiled).toBe(true);
    if (result.compiled) {
      expect(result.stats.statesCompiled).toBe(2);
      expect(result.stateMachine.states).toHaveLength(2);
    }
  });

  it("allows the same element across two different specs", () => {
    // Per-spec scoping means a "Severity" button on the EM spec and a
    // "Severity" button on the Findings spec do NOT collide — different
    // specs are different scopes, even when the elements look identical.
    const inputs = [
      { specId: "em", config: makeSingleStateSpec("em", "em-filtered", "Severity") },
      {
        specId: "findings",
        config: makeSingleStateSpec("findings", "findings-display", "Severity"),
      },
    ];
    const result = compileStateMachineFromSpecs(inputs);
    expect(result.compiled).toBe(true);
    if (result.compiled) {
      expect(result.stats.statesCompiled).toBe(2);
      expect(result.stateMachine.states).toHaveLength(2);
    }
  });

  it("quarantines the compilation when two states inside one spec share an element", () => {
    // Two states in the SAME spec both claim a button with accessibleName
    // "Severity" → one element-key, two state IDs in the same scope →
    // intra-spec collision → quarantine.
    const config = makeSpec("em", [
      { stateId: "em-filtered", label: "Severity" },
      { stateId: "em-summary", label: "Severity" },
    ]);
    const result = compileStateMachineFromSpecs([{ specId: "em", config }]);
    expect(result.compiled).toBe(false);
    if (!result.compiled) {
      expect(result.reason).toBe("quarantined");
      expect(result.quarantine.reason).toBe("duplicate-elements");
      expect(result.quarantine.conflicts).toHaveLength(1);
      const conflict = result.quarantine.conflicts[0];
      expect(conflict.elementKey).toContain("Severity");
      const stateIds = conflict.states.map((s) => s.stateId).sort();
      expect(stateIds).toEqual(["em-filtered", "em-summary"]);
      // Original spec set is preserved so operators can inspect offline.
      expect(result.quarantine.specs).toHaveLength(1);
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
      // Two specs each with their OWN intra-spec duplicate. Under the new
      // per-spec rule that produces 2 conflicts (one per spec), but still
      // exactly one aggregate console.warn call.
      const inputs = [
        {
          specId: "a",
          config: makeSpec("a", [
            { stateId: "sa1", label: "Dup" },
            { stateId: "sa2", label: "Dup" },
          ]),
        },
        {
          specId: "b",
          config: makeSpec("b", [
            { stateId: "sb1", label: "OtherDup" },
            { stateId: "sb2", label: "OtherDup" },
          ]),
        },
      ];
      const result = compileStateMachineFromSpecs(inputs);
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

  it("still throws when two observed/ai-fallback states inside one spec collide (hard invariant)", () => {
    // The stricter enforcement is preserved: observed/ai-fallback collisions
    // are an authoritative-source disagreement and throw synchronously so
    // the bug is surfaced immediately rather than buried in a quarantine.
    // Per-spec scoping means both observed states must live in the SAME
    // spec to trigger the throw.
    const config = makeSpec("a", [
      { stateId: "sa", label: "Claim" },
      { stateId: "sb", label: "Claim" },
    ]);
    // Force both states to "observed" via a discovery artifact that matches
    // either one by cluster-size proximity + support/contrast thresholds.
    expect(() =>
      compileStateMachineFromSpecs([{ specId: "a", config }], {
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
