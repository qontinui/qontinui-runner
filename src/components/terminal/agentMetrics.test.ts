/**
 * The metrics store and its projection (plan
 * `2026-09-20-terminal-session-state-comes-from-events-not-screen-scraping`,
 * Phase 7): wire mapping, the store's event + initial-load bridge, and the
 * absent / stale / source rules the cards render from.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const listenMock = vi.fn();
const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/event", () => ({ listen: (...a: unknown[]) => listenMock(...a) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));

import {
  ABSENT,
  CONTEXT_STALE_AFTER_MS,
  HEADROOM_STALE_AFTER_MS,
  TERMINAL_AGENT_METRICS_EVENT,
  applyAgentMetrics,
  getAgentMetrics,
  metricCells,
  normalizeSessionMetrics,
  parseAgentMetricsEvent,
  resetAgentMetricsStore,
  subscribeAgentMetrics,
  type SessionMetrics,
} from "./agentMetrics";

const NOW = 1_780_000_000_000;

function metrics(over: Partial<SessionMetrics> = {}): SessionMetrics {
  return {
    contextUsedPct: 42,
    contextTokens: 84_000,
    contextWindow: 200_000,
    contextSource: "transcript",
    contextObservedAtMs: NOW - 30_000,
    costUsd: null,
    fiveHourPct: 63,
    fiveHourResetsAt: "2026-09-30T18:00:00Z",
    sevenDayPct: 20,
    headroomSource: "oauth_probe",
    headroomObservedAtMs: NOW - 3 * 60_000,
    ...over,
  };
}

beforeEach(() => {
  listenMock.mockReset().mockResolvedValue(() => {});
  invokeMock.mockReset().mockResolvedValue([]);
  resetAgentMetricsStore();
});

afterEach(() => resetAgentMetricsStore());

describe("normalizeSessionMetrics", () => {
  it("keeps well-typed fields verbatim", () => {
    expect(normalizeSessionMetrics(metrics())).toEqual(metrics());
  });

  it("maps missing, mistyped and non-finite fields to null (absent), never 0", () => {
    const m = normalizeSessionMetrics({
      contextUsedPct: "42",
      contextTokens: Number.NaN,
      contextSource: "osc",
      headroomSource: "statusline",
      fiveHourResetsAt: "",
    });
    expect(Object.values(m).every((v) => v === null)).toBe(true);
  });

  it("tolerates a non-object", () => {
    expect(normalizeSessionMetrics(null).contextUsedPct).toBeNull();
  });
});

describe("parseAgentMetricsEvent", () => {
  it("rejects a payload without a terminal id or metrics object", () => {
    expect(parseAgentMetricsEvent({ metrics: metrics() })).toBeNull();
    expect(parseAgentMetricsEvent({ terminalId: "", metrics: metrics() })).toBeNull();
    expect(parseAgentMetricsEvent({ terminalId: "t1" })).toBeNull();
    expect(parseAgentMetricsEvent("nope")).toBeNull();
  });

  it("accepts and normalizes a well-formed payload", () => {
    const parsed = parseAgentMetricsEvent({ terminalId: "t1", metrics: { contextUsedPct: 5 } });
    expect(parsed?.terminalId).toBe("t1");
    expect(parsed?.metrics.contextUsedPct).toBe(5);
    expect(parsed?.metrics.fiveHourPct).toBeNull();
  });
});

describe("store", () => {
  it("applies a payload and notifies subscribers; malformed ones are ignored", () => {
    const listener = vi.fn();
    const unsub = subscribeAgentMetrics(listener);
    expect(applyAgentMetrics({ terminalId: "t1", metrics: metrics() })).toBe(true);
    expect(getAgentMetrics("t1")?.contextUsedPct).toBe(42);
    expect(listener).toHaveBeenCalledTimes(1);
    expect(applyAgentMetrics({ bogus: true })).toBe(false);
    expect(listener).toHaveBeenCalledTimes(1);
    expect(getAgentMetrics("t2")).toBeNull();
    expect(getAgentMetrics(null)).toBeNull();
    unsub();
  });

  it("first subscriber listens to the event and loads every row once", async () => {
    invokeMock.mockResolvedValue([
      { terminalId: "a", metrics: metrics({ contextUsedPct: 10 }) },
      { terminalId: "b", metrics: metrics({ contextUsedPct: 20 }) },
      { junk: 1 },
    ]);
    const unsub = subscribeAgentMetrics(() => {});
    subscribeAgentMetrics(() => {})();
    expect(listenMock).toHaveBeenCalledTimes(1);
    expect(listenMock.mock.calls[0][0]).toBe(TERMINAL_AGENT_METRICS_EVENT);
    expect(invokeMock).toHaveBeenCalledWith("get_terminal_agent_metrics");
    await vi.waitFor(() => expect(getAgentMetrics("b")?.contextUsedPct).toBe(20));
    expect(getAgentMetrics("a")?.contextUsedPct).toBe(10);

    // An event delivered through the listener lands in the store.
    const handler = listenMock.mock.calls[0][1] as (e: { payload: unknown }) => void;
    handler({ payload: { terminalId: "a", metrics: metrics({ contextUsedPct: 77 }) } });
    expect(getAgentMetrics("a")?.contextUsedPct).toBe(77);
    unsub();
  });

  it("swallows a runner without the command or the event", async () => {
    invokeMock.mockRejectedValue(new Error("unknown command"));
    listenMock.mockRejectedValue(new Error("no bridge"));
    const unsub = subscribeAgentMetrics(() => {});
    await new Promise((r) => setTimeout(r, 0));
    expect(getAgentMetrics("a")).toBeNull();
    unsub();
  });
});

describe("metricCells", () => {
  it("renders a fresh reading with its source and age in the title", () => {
    const c = metricCells(metrics(), NOW);
    expect(c.context).toMatchObject({ text: "42%", absent: false, stale: false, source: "transcript", age: "30s" });
    expect(c.context.title).toContain("84,000 / 200,000 tokens");
    expect(c.context.title).toContain("from transcript, 30s ago");
    expect(c.headroom).toMatchObject({ text: "63%", stale: false, source: "OAuth usage probe", age: "3m" });
    expect(c.headroom.title).toContain("37% headroom");
    expect(c.headroom.title).toContain("7-day window 20% used");
  });

  it("renders every cell absent ('—') when the runner reported nothing", () => {
    const c = metricCells(null, NOW);
    for (const cell of [c.context, c.cost, c.headroom]) {
      expect(cell.text).toBe(ABSENT);
      expect(cell.absent).toBe(true);
      expect(cell.stale).toBe(false);
    }
  });

  it("session cost is absent today — null is '—', never $0.00", () => {
    const c = metricCells(metrics({ costUsd: null }), NOW);
    expect(c.cost.text).toBe(ABSENT);
    expect(c.cost.title).toContain("no source");
  });

  it("a zero reading is a reading, not an absence", () => {
    const c = metricCells(metrics({ contextUsedPct: 0, fiveHourPct: 0 }), NOW);
    expect(c.context.text).toBe("0%");
    expect(c.headroom.text).toBe("0%");
  });

  it("derives the percentage from tokens when only those were reported", () => {
    const c = metricCells(metrics({ contextUsedPct: null, contextSource: "grid" }), NOW);
    expect(c.context.text).toBe("42%");
    expect(c.context.source).toBe("screen scan");
  });

  it("marks a reading past its threshold stale, keeping value and age", () => {
    const c = metricCells(
      metrics({
        contextObservedAtMs: NOW - CONTEXT_STALE_AFTER_MS - 60_000,
        headroomObservedAtMs: NOW - HEADROOM_STALE_AFTER_MS - 60_000,
      }),
      NOW,
    );
    expect(c.context).toMatchObject({ text: "42%", stale: true, age: "11m" });
    expect(c.context.title).toContain("(stale)");
    expect(c.headroom).toMatchObject({ text: "63%", stale: true, age: "21m" });
  });

  it("a reading of unknown age is stale, never silently fresh", () => {
    const c = metricCells(metrics({ contextObservedAtMs: null }), NOW);
    expect(c.context.stale).toBe(true);
    expect(c.context.age).toBeNull();
    expect(c.context.title).toContain("age unknown");
  });
});
