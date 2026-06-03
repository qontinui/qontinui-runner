/**
 * Unit tests for `computeStatusCounts` — the pure bucketing behind the
 * Terminal-page StatusStrip pills.
 *
 * Regression target: the strip used to count EVERY on-disk transcript
 * (`allSessions.length`), inflating the operator's real "3 sessions / 2 idle"
 * to "18 sessions / 14 idle" (and a fresh runner to "438 / 436"). The fix
 * drops `dormant` historical transcripts and orphaned (tab-less) `frozen`
 * transcripts, counting only the live / actively-managed set.
 */

import { describe, it, expect } from "vitest";
import { computeStatusCounts, type StatusCountsInput } from "./useSessionManager";

// Small builder so each case reads as a list of (status, orphaned) tuples.
function s(liveStatus: StatusCountsInput["liveStatus"], isOrphaned = false): StatusCountsInput {
  return { liveStatus, isOrphaned };
}

describe("computeStatusCounts", () => {
  it("excludes a dormant session from the total and from idle", () => {
    const counts = computeStatusCounts([s("dormant")]);
    expect(counts.sessionCount).toBe(0);
    expect(counts.idleCount).toBe(0);
    expect(counts.workingCount).toBe(0);
  });

  it("excludes an orphaned (tab-less) frozen transcript", () => {
    const counts = computeStatusCounts([s("frozen", /* isOrphaned */ true)]);
    expect(counts.sessionCount).toBe(0);
    expect(counts.idleCount).toBe(0);
  });

  it("counts a tab-backed (non-orphan) frozen session as idle", () => {
    const counts = computeStatusCounts([s("frozen", /* isOrphaned */ false)]);
    expect(counts.idleCount).toBe(1);
    expect(counts.sessionCount).toBe(1);
    expect(counts.workingCount).toBe(0);
  });

  it("counts active-in-zone and active-external as working", () => {
    const counts = computeStatusCounts([s("active-in-zone"), s("active-external")]);
    expect(counts.workingCount).toBe(2);
    expect(counts.idleCount).toBe(0);
    expect(counts.sessionCount).toBe(2);
  });

  it("counts needs-input toward need-input and the total", () => {
    const counts = computeStatusCounts([s("needs-input")]);
    // needs-input survives the dormant/orphan filter → contributes to the total
    expect(counts.sessionCount).toBe(1);
    expect(counts.workingCount).toBe(0);
    expect(counts.idleCount).toBe(0);
  });

  it("counts error and completed in their own buckets and the total", () => {
    const counts = computeStatusCounts([s("error"), s("completed")]);
    expect(counts.errorCount).toBe(1);
    expect(counts.completedCount).toBe(1);
    expect(counts.sessionCount).toBe(2);
  });

  it("operator's exact scenario: 1 working + 2 idle + N historical ⇒ total=3", () => {
    const sessions: StatusCountsInput[] = [
      // 1 genuinely working session
      s("active-in-zone"),
      // 2 idle = tab-backed (non-orphan) frozen sessions still open here
      s("frozen", /* isOrphaned */ false),
      s("frozen", /* isOrphaned */ false),
      // N historical noise that must NOT inflate the strip:
      // dormant never-reopened transcripts ...
      s("dormant"),
      s("dormant"),
      s("dormant"),
      // ... and orphaned (tab-less) frozen historical transcripts.
      s("frozen", /* isOrphaned */ true),
      s("frozen", /* isOrphaned */ true),
    ];
    const counts = computeStatusCounts(sessions);
    expect(counts.sessionCount).toBe(3);
    expect(counts.workingCount).toBe(1);
    expect(counts.idleCount).toBe(2);
    // 0 needs-input in this scenario; total must reconcile with the pills.
    expect(
      counts.workingCount +
        counts.idleCount +
        counts.completedCount +
        counts.errorCount,
    ).toBe(counts.sessionCount);
  });

  it("returns all-zero for an empty session list", () => {
    const counts = computeStatusCounts([]);
    expect(counts).toEqual({
      sessionCount: 0,
      workingCount: 0,
      idleCount: 0,
      completedCount: 0,
      errorCount: 0,
    });
  });
});
