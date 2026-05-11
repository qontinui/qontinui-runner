/**
 * Pure-helper tests for `MidSessionToast`.
 *
 * The runner's vitest config uses `environment: "node"` — JSX rendering
 * isn't available. Following the precedent set by
 * `LaunchMenu.test.tsx` and `CommitTrafficLight.test.tsx`, we exercise
 * the load-bearing logic via the exported pure helper
 * `formatToastSummary`. The JSX glue is verified manually + via UI
 * Bridge in Phase 4.
 */

import { describe, it, expect } from "vitest";
import { formatToastSummary } from "./MidSessionToast";
import type { MidSessionProbeState } from "./useMidSessionProbe";
import type { PredictedCollision } from "./useSessionManager";

function makeCollision(
  file_path: string,
  holders: Array<{ task_run_id: string; holder_name: string }>,
): PredictedCollision {
  return {
    file_path,
    other_holders: holders.map((h) => ({ ...h, registered_at: 0 })),
    confidence: 0.7,
  };
}

function makeState(collisions: PredictedCollision[]): MidSessionProbeState {
  return { predicted_collisions: collisions, received_at: 0 };
}

describe("formatToastSummary", () => {
  it("pluralizes a single collision held by one session", () => {
    const summary = formatToastSummary(
      makeState([
        makeCollision("src/foo.ts", [{ task_run_id: "t1", holder_name: "alpha" }]),
      ]),
    );
    expect(summary.header).toBe("1 file held by another session");
  });

  it("pluralizes multiple collisions across multiple sessions", () => {
    const summary = formatToastSummary(
      makeState([
        makeCollision("src/a.ts", [{ task_run_id: "t1", holder_name: "alpha" }]),
        makeCollision("src/b.ts", [{ task_run_id: "t2", holder_name: "beta" }]),
        makeCollision("src/c.ts", [{ task_run_id: "t3", holder_name: "gamma" }]),
      ]),
    );
    expect(summary.header).toBe("3 files held by other sessions");
  });

  it("uses 'other sessions' when a single file has multiple holders", () => {
    // Singular file count but plural holders → header should hint at
    // multiple sessions so the user knows it's a contested file.
    const summary = formatToastSummary(
      makeState([
        makeCollision("src/foo.ts", [
          { task_run_id: "t1", holder_name: "alpha" },
          { task_run_id: "t2", holder_name: "beta" },
        ]),
      ]),
    );
    expect(summary.header).toBe("1 file held by other sessions");
  });

  it("derives per-file rows with basename + first holder + joined holders", () => {
    const summary = formatToastSummary(
      makeState([
        makeCollision("D:/qontinui-root/src/foo.ts", [
          { task_run_id: "t1", holder_name: "alpha" },
          { task_run_id: "t2", holder_name: "beta" },
        ]),
        makeCollision("relative\\path\\bar.rs", [
          { task_run_id: "t3", holder_name: "gamma" },
        ]),
      ]),
    );
    expect(summary.rows).toHaveLength(2);

    expect(summary.rows[0]).toEqual({
      file_path: "D:/qontinui-root/src/foo.ts",
      basename: "foo.ts",
      holder_name: "alpha",
      holders_label: "alpha, beta",
    });

    // Backslash path uses Windows-style separator — basename must handle
    // both \\ and / (mixed paths come from the runner's path extractor).
    expect(summary.rows[1]).toEqual({
      file_path: "relative\\path\\bar.rs",
      basename: "bar.rs",
      holder_name: "gamma",
      holders_label: "gamma",
    });
  });

  it("falls back to 'unknown' when other_holders is empty", () => {
    // Defensive shape — runner filters out the caller's own holder
    // before serializing, so a 0-holder row is unusual but possible.
    const summary = formatToastSummary(
      makeState([makeCollision("src/foo.ts", [])]),
    );
    expect(summary.rows[0].holder_name).toBe("unknown");
    expect(summary.rows[0].holders_label).toBe("unknown");
  });
});
