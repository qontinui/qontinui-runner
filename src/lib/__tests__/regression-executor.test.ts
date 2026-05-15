/**
 * Tests for `runRegressionSuite`.
 *
 * End-to-end smoke test of the Phase A3 executor. Builds a tiny IR + suite,
 * mocks the UI Bridge registry + `@tauri-apps/api/core` `invoke`, and verifies
 * each of the four assertion discriminants dispatch correctly + the
 * persistence calls fire with the expected payloads.
 */

import { describe, it, expect, vi, beforeEach } from "vitest";

import { generateRegressionSuite } from "@qontinui/ui-bridge-auto/regression";
import type { IRDocument } from "@qontinui/shared-types/ui-bridge-ir";
import type { QueryableElement, RegistryLike } from "@qontinui/ui-bridge-auto/runtime";
import type { ScreenshotAssertionManager } from "@qontinui/ui-bridge-auto/visual";

import { makeTestAssertion } from "../__test-helpers__/ir-fixtures";

// ---------------------------------------------------------------------------
// Tauri invoke mock — must be set up before importing the executor module.
// ---------------------------------------------------------------------------

const invokeCalls: Array<{ cmd: string; args: unknown }> = [];
let invokeResults: Record<string, unknown> = {};

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args: unknown) => {
    invokeCalls.push({ cmd, args });
    if (cmd in invokeResults) return invokeResults[cmd];
    throw new Error(`unmocked tauri command: ${cmd}`);
  }),
}));

// `runRegressionSuite` must be imported *after* the mock is registered.
import { runRegressionSuite } from "../regression-executor";

// ---------------------------------------------------------------------------
// Fixture: tiny IR
// ---------------------------------------------------------------------------

const FIXTURE_IR: IRDocument = {
  version: "1.0",
  id: "fixture-ir",
  name: "Fixture",
  states: [
    {
      id: "state-a",
      name: "State A",
      assertions: [makeTestAssertion("state-a", 0, { role: "button", text: "Submit" })],
      provenance: { source: "build-plugin", file: "src/page-a.tsx" },
    },
    {
      id: "state-b",
      name: "State B",
      assertions: [makeTestAssertion("state-b", 0, { role: "heading", text: "Welcome" })],
      provenance: { source: "build-plugin", file: "src/page-b.tsx" },
    },
  ],
  transitions: [
    {
      id: "t1",
      name: "go A→B",
      fromStates: ["state-a"],
      activateStates: ["state-b"],
      actions: [
        {
          type: "click",
          target: { role: "button", text: "Submit" },
        },
      ],
      provenance: { source: "build-plugin", file: "src/page-a.tsx" },
    },
  ],
};

// ---------------------------------------------------------------------------
// Fake registry
// ---------------------------------------------------------------------------

function makeElement(id: string, type: string, label: string, visible = true): QueryableElement {
  // The matcher in `core/element-query.ts` reads `el.element.tagName`,
  // `el.element.textContent`, and `el.element.getAttribute(...)`. Provide a
  // minimal stand-in for an HTMLElement so the matcher can run without a real
  // DOM. `tagName` mirrors `type` (browsers report tagName upper-case).
  const fakeAttrs: Record<string, string | null> = {};
  const html = {
    tagName: type.toUpperCase(),
    nodeName: type.toUpperCase(),
    textContent: label,
    getAttribute: (name: string) =>
      Object.prototype.hasOwnProperty.call(fakeAttrs, name) ? fakeAttrs[name] : null,
    setAttribute: (name: string, value: string) => {
      fakeAttrs[name] = value;
    },
    parentElement: null,
    children: [],
    dataset: {},
  } as unknown as HTMLElement;
  return {
    id,
    element: html,
    type,
    label,
    getState: () => ({
      visible,
      enabled: true,
      textContent: label,
    }),
  };
}

function makeRegistry(elements: QueryableElement[]): RegistryLike {
  return {
    getAllElements: () => elements,
    on: () => () => {},
  };
}

// ---------------------------------------------------------------------------
// Stub ScreenshotAssertionManager — visual-gate isn't exercised in this suite
// (no baselineStore option => generator emits no visual-gate assertions),
// but pass a stub anyway so the executor never tries to construct the real
// TauriBaselineStore-backed manager during a unit test.
// ---------------------------------------------------------------------------

const stubScreenshotManager = {
  assertMatchesBaseline: vi.fn(async () => ({
    pass: true,
    diffPercentage: 0,
    diffPixelCount: 0,
    totalPixels: 1,
    baselineKey: "stub",
  })),
  captureBaseline: vi.fn(async () => null),
  assertElementsMatch: vi.fn(async () => ({
    pass: true,
    diffPercentage: 0,
    diffPixelCount: 0,
    totalPixels: 1,
    baselineKey: "stub",
  })),
} as unknown as ScreenshotAssertionManager;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("runRegressionSuite", () => {
  beforeEach(() => {
    invokeCalls.length = 0;
    invokeResults = {
      save_regression_suite: "11111111-1111-1111-1111-111111111111",
      record_regression_run: "22222222-2222-2222-2222-222222222222",
      record_assertion_executions_batch: 0,
      record_regression_diagnosis: "33333333-3333-3333-3333-333333333333",
    };
  });

  it("passes every assertion when the registry satisfies the IR", async () => {
    const suite = generateRegressionSuite(FIXTURE_IR);
    const elements = [
      makeElement("e1", "button", "Submit"),
      makeElement("e2", "heading", "Welcome"),
    ];
    const registry = makeRegistry(elements);

    const result = await runRegressionSuite({
      suite,
      ir: FIXTURE_IR,
      registry,
      irDocId: FIXTURE_IR.id,
      runId: "run-1",
      recentCommits: [],
      session: { id: "s1", startedAt: 0, events: [] },
      screenshotManager: stubScreenshotManager,
      now: () => 1_700_000_000_000,
    });

    expect(result.runResult.failed).toBe(0);
    // Total: 2 pre-state + 1 post-state + 1 action-target = 4 assertions
    // One transition has 1 fromState (state-a) -> 1 pre, 1 activateState
    // (state-b) -> 1 post, 1 action -> 1 action-target. So 3 total.
    expect(result.runResult.passed).toBe(3);
    expect(result.diagnosis).toBeNull();

    // Persistence calls fired in order.
    const cmds = invokeCalls.map((c) => c.cmd);
    expect(cmds).toEqual([
      "save_regression_suite",
      "record_regression_run",
      "record_assertion_executions_batch",
    ]);
  });

  it("fails state-active when a required element is missing", async () => {
    const suite = generateRegressionSuite(FIXTURE_IR);
    // Drop the heading: state-b's post-state assertion will fail.
    const elements = [makeElement("e1", "button", "Submit")];
    const registry = makeRegistry(elements);

    const result = await runRegressionSuite({
      suite,
      ir: FIXTURE_IR,
      registry,
      irDocId: FIXTURE_IR.id,
      runId: "run-2",
      recentCommits: [],
      session: { id: "s2", startedAt: 0, events: [] },
      screenshotManager: stubScreenshotManager,
      now: () => 1_700_000_000_000,
    });

    expect(result.runResult.failed).toBeGreaterThanOrEqual(1);
    const failureKinds = result.runResult.failures.map((f) => f.assertion.kind);
    expect(failureKinds).toContain("state-active");
    // Because the run failed, we must have called diagnose + persisted it.
    expect(result.diagnosis).not.toBeNull();
    const cmds = invokeCalls.map((c) => c.cmd);
    expect(cmds).toContain("record_regression_diagnosis");
  });

  it("fails action-target-resolves when the action target is ambiguous", async () => {
    const suite = generateRegressionSuite(FIXTURE_IR);
    const elements = [
      makeElement("e1", "button", "Submit"),
      makeElement("e2", "button", "Submit"), // duplicate button => ambiguous
      makeElement("e3", "heading", "Welcome"),
    ];
    const registry = makeRegistry(elements);

    const result = await runRegressionSuite({
      suite,
      ir: FIXTURE_IR,
      registry,
      irDocId: FIXTURE_IR.id,
      runId: "run-3",
      recentCommits: [],
      session: { id: "s3", startedAt: 0, events: [] },
      screenshotManager: stubScreenshotManager,
      now: () => 1_700_000_000_000,
    });

    const actionTargetFailures = result.runResult.failures.filter(
      (f) => f.assertion.kind === "action-target-resolves",
    );
    expect(actionTargetFailures.length).toBe(1);
  });

  it("dispatches overlay assertions and skips unknown overlay ids", async () => {
    // Hand-build a suite that includes an unknown overlay id so we exercise
    // the forward-compatibility path. We can't generate it via overlays —
    // overlays produce known ids — so we patch the suite directly.
    const suite = generateRegressionSuite(FIXTURE_IR);
    suite.cases[0]!.assertions.push({
      kind: "overlay",
      overlayId: "future-overlay-not-yet-implemented",
      assertionId: "future#0",
      payload: {},
    });
    // Plus a known overlay (`visibility`) targeting state-a's button.
    suite.cases[0]!.assertions.push({
      kind: "overlay",
      overlayId: "visibility",
      assertionId: "state-a#0",
      payload: { stateId: "state-a", requiredElementIndex: 0, minRatio: 1 },
    });

    const elements = [
      makeElement("e1", "button", "Submit"),
      makeElement("e2", "heading", "Welcome"),
    ];
    const registry = makeRegistry(elements);

    const result = await runRegressionSuite({
      suite,
      ir: FIXTURE_IR,
      registry,
      irDocId: FIXTURE_IR.id,
      runId: "run-4",
      recentCommits: [],
      session: { id: "s4", startedAt: 0, events: [] },
      screenshotManager: stubScreenshotManager,
      now: () => 1_700_000_000_000,
    });

    // No failures: visibility passes (button is visible), unknown overlay skips.
    expect(result.runResult.failed).toBe(0);
    expect(result.diagnosis).toBeNull();

    // The skip row must have made it into the batch payload.
    const batchCall = invokeCalls.find((c) => c.cmd === "record_assertion_executions_batch");
    expect(batchCall).toBeDefined();
    const executions = (batchCall!.args as { executions: unknown[] }).executions;
    const skippedRows = (executions as Array<{ status: string; assertion_kind: string }>).filter(
      (r) => r.status === "skip",
    );
    expect(skippedRows.length).toBeGreaterThanOrEqual(1);
  });

  it("threads the runDbId onto every assertion-execution row", async () => {
    const suite = generateRegressionSuite(FIXTURE_IR);
    const elements = [
      makeElement("e1", "button", "Submit"),
      makeElement("e2", "heading", "Welcome"),
    ];
    const registry = makeRegistry(elements);

    await runRegressionSuite({
      suite,
      ir: FIXTURE_IR,
      registry,
      irDocId: FIXTURE_IR.id,
      runId: "run-5",
      recentCommits: [],
      session: { id: "s5", startedAt: 0, events: [] },
      screenshotManager: stubScreenshotManager,
      now: () => 1_700_000_000_000,
    });

    const batchCall = invokeCalls.find((c) => c.cmd === "record_assertion_executions_batch");
    expect(batchCall).toBeDefined();
    const executions = (batchCall!.args as { executions: Array<{ run_id: string }> }).executions;
    expect(executions.length).toBeGreaterThan(0);
    for (const row of executions) {
      expect(row.run_id).toBe("22222222-2222-2222-2222-222222222222");
    }
  });
});
