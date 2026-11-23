/**
 * `get-roster-resync-stats` action wiring (plan
 * 2026-11-23-runner-terminal-page-popout-ui-bridge-and-resync-lag, D3).
 * Exercised through the pure factory under the node environment.
 */

import { afterEach, describe, expect, it } from "vitest";
import { componentSignatureId } from "@qontinui/ui-bridge";
import { buildRosterResyncStatsAction, ROSTER_RESYNC_STATS_ACTION_ID } from "./rosterResyncAction";
import {
  markResyncLogSince,
  recordResync,
  resetResyncLogForTest,
  type ResyncEntry,
} from "./terminalPagesSync";
import { ROSTER_RESYNC_BUDGET } from "./useTerminalPages";

const entry = (over: Partial<ResyncEntry> = {}): ResyncEntry => ({
  source: "main",
  receiver: "term-2",
  digest: "d",
  startedAt: 1_000,
  elapsedMs: 12,
  attempts: 1,
  outcome: "matched",
  ...over,
});

afterEach(() => resetResyncLogForTest());

describe("buildRosterResyncStatsAction", () => {
  it("is a declared read on terminal-page with its own signature", () => {
    const action = buildRosterResyncStatsAction(() => "main");
    expect(action.id).toBe(ROSTER_RESYNC_STATS_ACTION_ID);
    expect(action.id).toBe("get-roster-resync-stats");
    expect(action.effect).toBe("read");
    expect(action.signature?.id).toBe(componentSignatureId("terminal-page", action.id));
  });

  it("before any listener mounted: since null, entries [] (UNKNOWN, not a zero lag)", () => {
    const out = buildRosterResyncStatsAction(() => "term-2").handler();
    expect(out).toEqual({
      windowLabel: "term-2",
      since: null,
      budget: { retries: 5, retryMs: 50, finalRereadMs: 1000 },
      entries: [],
    });
  });

  it("returns the window label read at call time, since, the budget and the ring", () => {
    let label = "main";
    const action = buildRosterResyncStatsAction(() => label);
    markResyncLogSince(500);
    recordResync(entry());
    recordResync(entry({ outcome: "matched-late", attempts: 7, elapsedMs: 1250 }));
    label = "term-4";

    const out = action.handler();

    expect(out.windowLabel).toBe("term-4");
    expect(out.since).toBe(500);
    expect(out.budget).toEqual({
      retries: ROSTER_RESYNC_BUDGET.retries,
      retryMs: ROSTER_RESYNC_BUDGET.retryMs,
      finalRereadMs: ROSTER_RESYNC_BUDGET.finalRereadMs,
    });
    expect(out.entries.map((e) => e.outcome)).toEqual(["matched", "matched-late"]);
    expect(Object.keys(out).sort()).toEqual(["budget", "entries", "since", "windowLabel"]);
  });

  it("hands out a copy: mutating the answer does not change the ring", () => {
    recordResync(entry());
    const action = buildRosterResyncStatsAction(() => "main");
    const first = action.handler();
    first.entries[0].outcome = "exhausted";
    first.entries.length = 0;
    expect(action.handler().entries.map((e) => e.outcome)).toEqual(["matched"]);
  });
});
