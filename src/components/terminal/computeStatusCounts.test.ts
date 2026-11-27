/**
 * Unit tests for `computeStatusCounts` — the pure bucketing behind the
 * Terminal-page StatusStrip pills, over the two-axis status (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 1).
 *
 * Regression target: the strip used to count EVERY on-disk transcript
 * (`allSessions.length`), inflating the operator's real "3 sessions / 2 idle"
 * to "18 sessions / 14 idle" (and a fresh runner to "438 / 436"). A transcript
 * whose process is gone is now dropped by its LOCATION (`Not running`), and a
 * row that could not be placed (`Unknown`) is counted, never dropped.
 */

import { describe, it, expect } from "vitest";
import { computeStatusCounts } from "./sessionCounts";
import type { SessionActivity, SessionLocation } from "./sessionStatusModel";

function s(location: SessionLocation, activity: SessionActivity) {
  return { status: { location, activity } };
}

describe("computeStatusCounts", () => {
  it("excludes a not-running session from the total and every bucket", () => {
    const counts = computeStatusCounts([s("not-running", "exited"), s("not-running", "finished")]);
    expect(counts).toEqual({
      claudeSessionCount: 0,
      workingCount: 0,
      outsideWorkingCount: 0,
      idleCount: 0,
      completedCount: 0,
      errorCount: 0,
      unknownCount: 0,
    });
  });

  it("counts a session whose location is Unknown — could-not-tell is not gone", () => {
    const counts = computeStatusCounts([s("unknown", "unknown")]);
    expect(counts.claudeSessionCount).toBe(1);
    expect(counts.unknownCount).toBe(1);
  });

  it("counts working and waiting-on-lock as working, and carries the outside share", () => {
    const counts = computeStatusCounts([
      s("on-screen", "working"),
      s("hidden-here", "waiting-on-lock"),
      s("outside-runner", "working"),
    ]);
    expect(counts.workingCount).toBe(3);
    expect(counts.outsideWorkingCount).toBe(1);
  });

  it("counts idle-at-prompt and no-output-mid-tool as idle", () => {
    const counts = computeStatusCounts([
      s("hidden-here", "idle-at-prompt"),
      s("on-screen", "no-output-mid-tool"),
    ]);
    expect(counts.idleCount).toBe(2);
  });

  it("counts needs-you in the total only (its own pill reports it)", () => {
    const counts = computeStatusCounts([s("hidden-here", "needs-you")]);
    expect(counts.claudeSessionCount).toBe(1);
    expect(counts.workingCount + counts.idleCount + counts.unknownCount).toBe(0);
  });

  it("counts finished / exited as completed and exited-error as error", () => {
    const counts = computeStatusCounts([
      s("on-screen", "finished"),
      s("hidden-here", "exited"),
      s("hidden-here", "exited-error"),
    ]);
    expect(counts.completedCount).toBe(2);
    expect(counts.errorCount).toBe(1);
    expect(counts.claudeSessionCount).toBe(3);
  });

  it("operator's scenario: 1 working + 2 idle + 200 historical ⇒ total 3", () => {
    const historical = Array.from({ length: 200 }, () => s("not-running", "exited"));
    const counts = computeStatusCounts([
      s("on-screen", "working"),
      s("hidden-here", "idle-at-prompt"),
      s("hidden-here", "no-output-mid-tool"),
      ...historical,
    ]);
    expect(counts.claudeSessionCount).toBe(3);
    expect(counts.workingCount).toBe(1);
    expect(counts.idleCount).toBe(2);
  });

  it("returns all-zero for an empty session list", () => {
    expect(computeStatusCounts([]).claudeSessionCount).toBe(0);
  });
});
