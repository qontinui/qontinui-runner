/**
 * The proxy-ack replacement must actually carry state tracking (plan Phase 5 /
 * A4). These lock down the four surfaces the removed proxy-ack kept alive for a
 * tab with no mounted pane — staleness clock, activity sparkline,
 * `lastOutputLines`, state-chip detector text — now that the runner emits no
 * `terminal-output` for such a tab at all.
 */

import { describe, it, expect } from "vitest";
import {
  applyActivityDigest,
  DIGEST_OUTPUT_LINES,
  type DigestTrackingState,
} from "./activityDigestTracking";

function emptyState(): DigestTrackingState {
  return { lastOutputTime: {}, activityBuffers: {} };
}

describe("applyActivityDigest", () => {
  it("feeds all four tracking surfaces from one digest", () => {
    const state = emptyState();
    const effects = applyActivityDigest(state, "t1", 4096, ["$ ls", "a  b  c"], 1_700_000_000_000);

    expect(state.lastOutputTime.t1).toBe(1_700_000_000_000);
    expect(state.activityBuffers.t1).toEqual([4096]);
    expect(effects.outputLines).toEqual(["$ ls", "a  b  c"]);
    expect(effects.detectorText).toBe("$ ls\na  b  c");
  });

  it("accumulates PTY bytes, not digest text length, into the sparkline", () => {
    // The load-bearing difference from the tap: the digest carries a 20-line
    // screen tail whose length says nothing about throughput. An unwatched
    // tab's bars must stay comparable with a focused tab's.
    const state = emptyState();
    applyActivityDigest(state, "t1", 100_000, ["tiny"], 1);
    expect(state.activityBuffers.t1).toEqual([100_000]);
  });

  it("adds into the OPEN bucket so digests and live chunks share a window", () => {
    const state: DigestTrackingState = {
      lastOutputTime: {},
      // Two closed buckets and one open one, as the 2 s rotation leaves it.
      activityBuffers: { t1: [12, 34, 5] },
    };
    applyActivityDigest(state, "t1", 60, ["x"], 1);
    expect(state.activityBuffers.t1).toEqual([12, 34, 65]);
  });

  it("keeps only the trailing screen tail", () => {
    const state = emptyState();
    const lines = Array.from({ length: 50 }, (_, i) => `line ${i}`);
    const effects = applyActivityDigest(state, "t1", 1, lines, 1);
    expect(effects.outputLines).toHaveLength(DIGEST_OUTPUT_LINES);
    expect(effects.outputLines?.[DIGEST_OUTPUT_LINES - 1]).toBe("line 49");
  });

  it("records activity but publishes nothing when the digest has no lines", () => {
    // A burst of pure control bytes (a TUI repositioning the cursor) leaves the
    // rendered grid unchanged. The tab is demonstrably alive — so the staleness
    // clock and the sparkline must move — but blanking `lastOutputLines` would
    // wipe a compact card that still shows the right content.
    const state = emptyState();
    const effects = applyActivityDigest(state, "t1", 512, [], 42);
    expect(state.lastOutputTime.t1).toBe(42);
    expect(state.activityBuffers.t1).toEqual([512]);
    expect(effects.outputLines).toBeNull();
    expect(effects.detectorText).toBe("");
  });

  it("tracks tabs independently", () => {
    const state = emptyState();
    applyActivityDigest(state, "t1", 10, ["a"], 1);
    applyActivityDigest(state, "t2", 20, ["b"], 2);
    applyActivityDigest(state, "t1", 5, ["c"], 3);
    expect(state.activityBuffers).toEqual({ t1: [15], t2: [20] });
    expect(state.lastOutputTime).toEqual({ t1: 3, t2: 2 });
  });
});
