/**
 * Pure-helper tests for FileOwnershipHeatmap.
 *
 * The runner's vitest config uses `environment: "node"` (no jsdom), so we
 * follow the repo precedent (`CommitTrafficLight.test.tsx`,
 * `useCommitState.test.ts`) and exercise the exported pure functions
 * (`computeHeatmapRows`, `decayRecency`) instead of mounting React.
 *
 * Coverage:
 *   1. `computeHeatmapRows` flags `contended: true` when the same
 *      filePath appears with two distinct task_run_ids in the input
 *      (i.e., the consumer should render `border-l-2 border-l-[#f7768e]`).
 *   2. `computeHeatmapRows` leaves `contended: false` when filePaths are
 *      distinct (no contention class).
 *   3. Sort order — within the same contention bucket, more-recent rows
 *      come first; contended rows always precede non-contended rows.
 *   4. `decayRecency` is monotone: now → 1, far past → asymptote 0.
 *   5. `computeHeatmapRows` dedupes per (file, session) — repeated touches
 *      of the same path by the same session are NOT counted as
 *      contention.
 */

import { describe, it, expect } from "vitest";
import {
  computeHeatmapRows,
  decayRecency,
  type TouchedFileRow,
} from "./FileOwnershipHeatmap";

const NOW = 1_700_000_000_000; // arbitrary fixed epoch ms — keeps tests deterministic

const row = (
  file_path: string,
  task_run_id: string,
  msAgo: number,
): TouchedFileRow => ({
  file_path,
  task_run_id,
  recorded_at_ms: NOW - msAgo,
});

describe("computeHeatmapRows — contention detection", () => {
  it("flags contended when same filePath has two distinct task_run_ids", () => {
    const rows = computeHeatmapRows(
      [
        row("/repo/src/foo.rs", "session-A", 1_000),
        row("/repo/src/foo.rs", "session-B", 2_000),
      ],
      NOW,
    );

    expect(rows).toHaveLength(1);
    expect(rows[0].filePath).toBe("/repo/src/foo.rs");
    expect(rows[0].contended).toBe(true);
    expect(rows[0].taskRunIds).toHaveLength(2);
    // Most-recent session first within the row.
    expect(rows[0].taskRunIds[0]).toBe("session-A");
    expect(rows[0].taskRunIds[1]).toBe("session-B");
  });

  it("does NOT flag contention when filePaths are different", () => {
    const rows = computeHeatmapRows(
      [
        row("/repo/src/foo.rs", "session-A", 1_000),
        row("/repo/src/bar.rs", "session-B", 1_000),
      ],
      NOW,
    );

    expect(rows).toHaveLength(2);
    for (const r of rows) {
      expect(r.contended).toBe(false);
      expect(r.taskRunIds).toHaveLength(1);
    }
  });

  it("does NOT flag contention when the SAME session re-touches the same file", () => {
    // Repeated UPSERT-style writes from one session must not look like
    // multi-session contention.
    const rows = computeHeatmapRows(
      [
        row("/repo/src/foo.rs", "session-A", 1_000),
        row("/repo/src/foo.rs", "session-A", 2_000),
        row("/repo/src/foo.rs", "session-A", 3_000),
      ],
      NOW,
    );

    expect(rows).toHaveLength(1);
    expect(rows[0].contended).toBe(false);
    expect(rows[0].taskRunIds).toEqual(["session-A"]);
  });
});

describe("computeHeatmapRows — sort order", () => {
  it("places contended rows before non-contended rows", () => {
    const rows = computeHeatmapRows(
      [
        // Non-contended but very recent
        row("/repo/src/recent-solo.rs", "session-X", 100),
        // Contended but older
        row("/repo/src/old-shared.rs", "session-A", 5_000),
        row("/repo/src/old-shared.rs", "session-B", 6_000),
      ],
      NOW,
    );

    expect(rows).toHaveLength(2);
    expect(rows[0].filePath).toBe("/repo/src/old-shared.rs");
    expect(rows[0].contended).toBe(true);
    expect(rows[1].filePath).toBe("/repo/src/recent-solo.rs");
    expect(rows[1].contended).toBe(false);
  });

  it("within the same contention bucket, more-recent rows come first", () => {
    const rows = computeHeatmapRows(
      [
        // Three non-contended files, varying recency
        row("/repo/old.rs", "session-A", 10_000),
        row("/repo/newest.rs", "session-A", 100),
        row("/repo/middle.rs", "session-A", 5_000),
      ],
      NOW,
    );

    expect(rows.map((r) => r.filePath)).toEqual([
      "/repo/newest.rs",
      "/repo/middle.rs",
      "/repo/old.rs",
    ]);
    // Recencies must be strictly decreasing.
    expect(rows[0].recency).toBeGreaterThan(rows[1].recency);
    expect(rows[1].recency).toBeGreaterThan(rows[2].recency);
  });

  it("returns an empty list for an empty input", () => {
    expect(computeHeatmapRows([], NOW)).toEqual([]);
  });
});

describe("decayRecency", () => {
  it("returns 1 for a touch happening exactly at `now`", () => {
    expect(decayRecency(NOW, NOW)).toBeCloseTo(1, 10);
  });

  it("clamps to 1 when recordedAt is in the future (clock skew)", () => {
    expect(decayRecency(NOW + 5_000, NOW)).toBeCloseTo(1, 10);
  });

  it("decays toward 0 as the touch ages", () => {
    const rNow = decayRecency(NOW, NOW);
    const rOneMin = decayRecency(NOW - 60_000, NOW);
    const rTenMin = decayRecency(NOW - 600_000, NOW);
    const rOneHour = decayRecency(NOW - 3_600_000, NOW);

    expect(rNow).toBeGreaterThan(rOneMin);
    expect(rOneMin).toBeGreaterThan(rTenMin);
    expect(rTenMin).toBeGreaterThan(rOneHour);
    expect(rOneHour).toBeGreaterThan(0);
    // 10-minute touch lands near exp(-1) = 0.3679 with the documented tau.
    expect(rTenMin).toBeCloseTo(Math.exp(-1), 5);
  });
});
