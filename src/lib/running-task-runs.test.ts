/**
 * Tests for the `GET /task-runs/running` envelope reader.
 *
 * Plan `2026-08-29-no-single-answer-to-is-it-safe-to-restart-the-runner`
 * Phase 2/D4. These pin the SHAPE contract every frontend poller depends on:
 * the endpoint returns `{ scope, task_runs }`, and an unexpected body must
 * degrade to "no runs" rather than throw into the render tree (three of the
 * four consumers feed this straight into state that is iterated during render).
 */

import { describe, it, expect } from "vitest";

import { extractRunningTaskRuns, runningTaskRunsScope } from "./running-task-runs";

const SCOPE = "workflow task-runs on API port 9876; NOT a session census — see /restart-readiness";

describe("extractRunningTaskRuns", () => {
  it("returns the rows under `task_runs`", () => {
    const rows = extractRunningTaskRuns<{ id: string }>({
      scope: SCOPE,
      task_runs: [{ id: "run-a" }, { id: "run-b" }],
    });

    expect(rows).toHaveLength(2);
    expect(rows.map((r) => r.id)).toEqual(["run-a", "run-b"]);
  });

  it("returns [] for an empty ledger without losing the scope", () => {
    const body = { scope: SCOPE, task_runs: [] };

    expect(extractRunningTaskRuns(body)).toEqual([]);
    // The incident case: empty still says what it covers.
    expect(runningTaskRunsScope(body)).toBe(SCOPE);
  });

  it("does NOT accept a bare array — that shape no longer exists", () => {
    // Tolerating it would let a stale runner's response read as authoritative
    // and would keep the pre-2026-08-29 misreading alive in the client.
    expect(extractRunningTaskRuns([{ id: "run-a" }])).toEqual([]);
  });

  it("degrades to [] for null, undefined, scalars and a missing key", () => {
    expect(extractRunningTaskRuns(null)).toEqual([]);
    expect(extractRunningTaskRuns(undefined)).toEqual([]);
    expect(extractRunningTaskRuns("[]")).toEqual([]);
    expect(extractRunningTaskRuns(42)).toEqual([]);
    expect(extractRunningTaskRuns({ scope: SCOPE })).toEqual([]);
    expect(extractRunningTaskRuns({ scope: SCOPE, task_runs: null })).toEqual([]);
  });
});

describe("runningTaskRunsScope", () => {
  it("reports the endpoint's own scope string", () => {
    expect(runningTaskRunsScope({ scope: SCOPE, task_runs: [] })).toBe(SCOPE);
  });

  it("returns null rather than inventing one when it is absent or blank", () => {
    expect(runningTaskRunsScope({ task_runs: [] })).toBeNull();
    expect(runningTaskRunsScope({ scope: "", task_runs: [] })).toBeNull();
    expect(runningTaskRunsScope(null)).toBeNull();
  });
});
