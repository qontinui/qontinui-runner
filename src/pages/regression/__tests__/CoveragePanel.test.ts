/**
 * Tests for the pure helpers powering CoveragePanel.
 *
 * The runner repo is `environment: "node"` and does not bundle jsdom — the
 * convention (see `WebIntegrationAuthBanner.test.ts`) is to factor pure
 * decision logic out of components and lock it down here. The component
 * itself is a thin shell over `coverageDiff` / `coverageOf` / our exposed
 * adapters, so covering those adapters is sufficient to keep the panel
 * honest without spinning up a DOM harness.
 */

import { describe, it, expect } from "vitest";

import {
  coverageDiff,
  coverageOf,
  generateRegressionSuite,
  type RegressionSuite,
} from "@qontinui/ui-bridge-auto/regression";
import type { IRDocument } from "@qontinui/shared-types/ui-bridge-ir";

import {
  exerciseLogToAssertionExecutions,
  groupUnexercisedByCase,
  deriveCoverageStats,
  type AssertionExecutionRow,
} from "../CoveragePanel";

// ---------------------------------------------------------------------------
// Fixtures — minimal IR + suite generated from it. Mirrors the shape used in
// `regression-executor.test.ts` so a reader recognizes the conventions.
// ---------------------------------------------------------------------------

const FIXTURE_IR: IRDocument = {
  version: "1.0",
  id: "coverage-fixture",
  name: "Coverage Fixture",
  states: [
    {
      id: "s-root",
      name: "Root",
      requiredElements: [{ tagName: "body" }],
    },
    {
      id: "s-page",
      name: "Page",
      requiredElements: [{ role: "heading" }],
    },
  ],
  transitions: [
    {
      id: "t-go",
      name: "Go to page",
      fromStates: ["s-root"],
      activateStates: ["s-page"],
      actions: [{ type: "click", target: { role: "button", text: "Go" } }],
    },
  ],
  initialState: "s-root",
};

function buildSuite(): RegressionSuite {
  return generateRegressionSuite(FIXTURE_IR);
}

// ---------------------------------------------------------------------------
// exerciseLogToAssertionExecutions — Tauri row → coverage-diff input adapter
// ---------------------------------------------------------------------------

describe("exerciseLogToAssertionExecutions", () => {
  it("returns an empty array for an empty input", () => {
    expect(exerciseLogToAssertionExecutions([])).toEqual([]);
  });

  it("maps snake_case Rust rows onto camelCase AssertionExecution entries", () => {
    const rows: AssertionExecutionRow[] = [
      {
        case_id: "t-go",
        assertion_id: "state-active:pre:s-root",
        started_at: "2026-05-02T12:00:00Z",
        status: "passed",
        assertion_kind: "state-active",
        duration_ms: 4,
        failure_kind: null,
        error_message: null,
      },
      {
        case_id: "t-go",
        assertion_id: "action-target-resolves:t-go#0",
        started_at: "2026-05-02T12:00:01Z",
        status: "passed",
        assertion_kind: "action-target-resolves",
        duration_ms: 7,
      },
    ];

    expect(exerciseLogToAssertionExecutions(rows)).toEqual([
      {
        caseId: "t-go",
        assertionId: "state-active:pre:s-root",
        executedAt: "2026-05-02T12:00:00Z",
      },
      {
        caseId: "t-go",
        assertionId: "action-target-resolves:t-go#0",
        executedAt: "2026-05-02T12:00:01Z",
      },
    ]);
  });

  it("does not mutate the caller's input array", () => {
    const rows: AssertionExecutionRow[] = [
      {
        case_id: "t-go",
        assertion_id: "x",
        started_at: "2026-05-02T12:00:00Z",
        status: "passed",
        assertion_kind: "state-active",
        duration_ms: 1,
      },
    ];
    const snapshot = JSON.parse(JSON.stringify(rows));
    exerciseLogToAssertionExecutions(rows);
    expect(rows).toEqual(snapshot);
  });
});

// ---------------------------------------------------------------------------
// groupUnexercisedByCase — coverageDiff output → renderable groups
// ---------------------------------------------------------------------------

describe("groupUnexercisedByCase", () => {
  it("groups un-exercised assertions by caseId", () => {
    const suite = buildSuite();

    // Empty exercise log — every assertion should be un-exercised.
    const log: AssertionExecutionRow[] = [];
    const execs = exerciseLogToAssertionExecutions(log);

    const diff = coverageDiff(suite, execs);

    const groups = groupUnexercisedByCase(suite, diff);

    expect(groups).toHaveLength(1);
    expect(groups[0]!.caseId).toBe("t-go");
    expect(groups[0]!.rows.length).toBeGreaterThan(0);

    // Every emitted row should carry a kind that is one of the four discriminants
    // — never "unknown" (which would indicate the kind-lookup keying drifted
    // away from coverage-diff's `assertionIdOf` convention).
    for (const r of groups[0]!.rows) {
      expect(["state-active", "action-target-resolves", "visual-gate", "overlay"]).toContain(
        r.assertionKind,
      );
    }

    // Specific check: the pre-state assertion for s-root should appear.
    const preRoot = groups[0]!.rows.find(
      (r) => r.assertionId === "state-active:pre:s-root",
    );
    expect(preRoot).toBeDefined();
    expect(preRoot!.assertionKind).toBe("state-active");

    // Specific check: the action-target-resolves assertion should appear.
    const actionRes = groups[0]!.rows.find(
      (r) => r.assertionId === "action-target-resolves:t-go#0",
    );
    expect(actionRes).toBeDefined();
    expect(actionRes!.assertionKind).toBe("action-target-resolves");
  });

  it("returns no groups when every assertion has been exercised", () => {
    const suite = buildSuite();

    // Build a synthetic log that records every assertion in the suite as
    // having executed. The id derivation must match coverage-diff's
    // `assertionIdOf` convention.
    const rows: AssertionExecutionRow[] = [];
    for (const c of suite.cases) {
      for (const a of c.assertions) {
        let assertionId: string;
        switch (a.kind) {
          case "state-active":
            assertionId = `state-active:${a.phase}:${a.stateId}`;
            break;
          case "action-target-resolves":
            assertionId = `action-target-resolves:${a.transitionId}#${a.actionIndex}`;
            break;
          case "visual-gate":
            assertionId = `visual-gate:${a.stateId}`;
            break;
          case "overlay":
            assertionId = a.assertionId;
            break;
        }
        rows.push({
          case_id: c.id,
          assertion_id: assertionId,
          started_at: "2026-05-02T12:00:00Z",
          status: "passed",
          assertion_kind: a.kind,
          duration_ms: 1,
        });
      }
    }

    const execs = exerciseLogToAssertionExecutions(rows);
    const diff = coverageDiff(suite, execs);

    expect(diff.unexercisedAssertions).toEqual([]);
    expect(diff.uncoveredTransitions).toEqual([]);
    expect(groupUnexercisedByCase(suite, diff)).toEqual([]);
  });
});

// ---------------------------------------------------------------------------
// deriveCoverageStats — pull both sources together for the dashboard pills
// ---------------------------------------------------------------------------

describe("deriveCoverageStats", () => {
  it("combines coverageOf + coverageDiff into a renderable shape", () => {
    const suite = buildSuite();
    const staticCoverage = coverageOf(FIXTURE_IR, suite);
    const diff = coverageDiff(suite, []);

    const stats = deriveCoverageStats(staticCoverage, diff);

    // Suite has exactly one case (one transition), so static coverage is
    // 1/1 transitions.
    expect(stats.staticCovered).toBe(1);
    expect(stats.staticTotal).toBe(1);

    // Empty exercise log → zero exercised, all unexercised.
    expect(stats.runtimeExercised).toBe(0);
    expect(stats.runtimeTotal).toBeGreaterThan(0);
    expect(stats.uncoveredPercent).toBe(100);
  });

  it("rounds uncoveredRatio to a whole-number percent", () => {
    // Hand-crafted minimal CoverageReport + CoverageDiffReport — bypass the
    // generator so we can pin a non-trivial ratio.
    const stats = deriveCoverageStats(
      {
        totalStates: 2,
        totalTransitions: 1,
        statesCovered: 2,
        transitionsCovered: 1,
        reachableStates: ["s-root", "s-page"],
        unreachableStates: [],
      },
      {
        unexercisedAssertions: [],
        uncoveredTransitions: [],
        stats: {
          totalAssertions: 4,
          exercisedAssertions: 1,
          totalTransitions: 1,
          coveredTransitions: 1,
          uncoveredRatio: 0.75,
        },
      },
    );

    expect(stats.uncoveredPercent).toBe(75);
  });

  it("emits 0% when there are no assertions to cover", () => {
    const stats = deriveCoverageStats(
      {
        totalStates: 0,
        totalTransitions: 0,
        statesCovered: 0,
        transitionsCovered: 0,
        reachableStates: [],
        unreachableStates: [],
      },
      {
        unexercisedAssertions: [],
        uncoveredTransitions: [],
        stats: {
          totalAssertions: 0,
          exercisedAssertions: 0,
          totalTransitions: 0,
          coveredTransitions: 0,
          uncoveredRatio: 0,
        },
      },
    );

    expect(stats.uncoveredPercent).toBe(0);
  });
});
