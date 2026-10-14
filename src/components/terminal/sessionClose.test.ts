/**
 * D7 close semantics: every graceful-exit outcome maps to an honest per-item
 * result, a refusal never falls back to a kill, only what actually closed is
 * dropped and recorded, and Force close is its own path.
 */

import { describe, expect, it, vi } from "vitest";

import {
  closeNeedsSecondTick,
  closeSessionsGracefully,
  describeForceCloseOutcome,
  describeGracefulOutcome,
  forceCloseSessions,
  summarizeCloseResults,
  type CloseEffects,
  type CloseItem,
} from "./sessionClose";
import { SESSION_ACTIVITIES } from "./sessionStatusModel";

function item(id: string, tabId: string | null = `tab-${id}`, tabAlive = true): CloseItem {
  return {
    sessionId: id,
    title: `Session ${id}`,
    tabId,
    tabAlive,
    sessionKey: `key-${id}`,
    resumeCmd: `claude --resume ${id}`,
  };
}

function effects(outcomes: Record<string, unknown | Error>): CloseEffects & {
  gracefulClose: ReturnType<typeof vi.fn>;
  forceClose: ReturnType<typeof vi.fn>;
  dropTab: ReturnType<typeof vi.fn>;
  recordClosed: ReturnType<typeof vi.fn>;
} {
  return {
    gracefulClose: vi.fn(async (tabId: string) => {
      const o = outcomes[tabId];
      if (o instanceof Error) throw o;
      return o;
    }),
    forceClose: vi.fn(),
    dropTab: vi.fn(),
    recordClosed: vi.fn(),
    now: () => 1000,
  };
}

describe("second tick", () => {
  it("is asked for exactly Needs you, Working and Unknown", () => {
    expect(SESSION_ACTIVITIES.filter(closeNeedsSecondTick)).toEqual([
      "needs-you",
      "working",
      "unknown",
    ]);
  });
});

describe("describeGracefulOutcome", () => {
  it.each([
    [{ outcome: "exited", waited_ms: 2400, claude_pids: [1] }, "closed", "after 2s"],
    [
      { outcome: "exit_stuck", waited_ms: 60000, claude_pids: [1], last_probe_unreadable: false },
      "not-closed",
      "nothing was killed",
    ],
    [
      { outcome: "refused", reason: "the prompt is not empty", claude_pids: [1] },
      "not-closed",
      "the prompt is not empty",
    ],
    [{ outcome: "no_live_claude" }, "not-closed", "Force close"],
    [{ outcome: "probe_unavailable", detail: "ps failed" }, "not-closed", "ps failed"],
    [{ outcome: "write_failed", error: "EPIPE", claude_pids: [] }, "not-closed", "EPIPE"],
    [
      { outcome: "close_refused", waited_ms: 1, claude_pids: [], reason: "relaunched" },
      "not-closed",
      "relaunched",
    ],
    [
      { outcome: "close_outcome_unknown", waited_ms: 1, claude_pids: [], detail: "task died" },
      "unknown",
      "task died",
    ],
    [{ outcome: "something_new" }, "unknown", "unrecognised"],
    [null, "unknown", "unrecognised"],
  ])("%j → %s", (raw, state, detail) => {
    const r = describeGracefulOutcome(raw);
    expect(r.state).toBe(state);
    expect(r.detail).toContain(detail);
    if (state !== "closed") expect(r.detail).not.toMatch(/^Closed/);
  });
});

describe("closeSessionsGracefully", () => {
  it("reports each item, drops and records ONLY the ones that closed", async () => {
    const fx = effects({
      "tab-a": { outcome: "exited", waited_ms: 1000, claude_pids: [1] },
      "tab-b": { outcome: "refused", reason: "busy", claude_pids: [2] },
      "tab-c": new Error("Terminal session not found: tab-c"),
      "tab-d": { outcome: "close_outcome_unknown", waited_ms: 1, claude_pids: [], detail: "x" },
    });
    const results = await closeSessionsGracefully(
      [item("a"), item("b"), item("c"), item("d"), item("e", null)],
      fx,
    );
    expect(results.map((r) => [r.sessionId, r.state])).toEqual([
      ["a", "closed"],
      ["b", "not-closed"],
      ["c", "not-closed"],
      ["d", "unknown"],
      ["e", "not-closed"],
    ]);
    expect(results[2].detail).toContain("Terminal session not found");
    expect(results[4].detail).toContain("no live tab");
    // A refusal never falls back to the kill path.
    expect(fx.forceClose).not.toHaveBeenCalled();
    expect(fx.gracefulClose.mock.calls.map((c) => c[0])).toEqual([
      "tab-a",
      "tab-b",
      "tab-c",
      "tab-d",
    ]);
    expect(fx.dropTab.mock.calls).toEqual([["tab-a"]]);
    expect(fx.recordClosed.mock.calls).toEqual([
      [
        [
          {
            op: "recordClosed",
            sessionKey: "key-a",
            title: "Session a",
            closedAt: 1000,
            resumeCmd: "claude --resume a",
          },
        ],
      ],
    ]);
    expect(summarizeCloseResults(results)).toBe("1 of 5 closed · 3 not closed · 1 unknown");
  });

  it("records nothing when nothing closed", async () => {
    const fx = effects({ "tab-a": { outcome: "no_live_claude" } });
    await closeSessionsGracefully([item("a")], fx);
    expect(fx.recordClosed).not.toHaveBeenCalled();
    expect(fx.dropTab).not.toHaveBeenCalled();
  });
});

describe("closing an exited session", () => {
  it("removes its tab without /exit or a kill, and records it closed", async () => {
    const fx = effects({});
    const results = await closeSessionsGracefully([item("x", "tab-x", false)], fx);
    expect(fx.gracefulClose).not.toHaveBeenCalled();
    expect(fx.forceClose).not.toHaveBeenCalled();
    expect(fx.dropTab.mock.calls).toEqual([["tab-x"]]);
    expect(results[0].state).toBe("closed");
    expect(results[0].detail).toContain("already exited");
    expect(fx.recordClosed.mock.calls[0][0]).toHaveLength(1);
  });

  it("Force close takes the same removal, never the kill", async () => {
    const fx = effects({});
    const results = await forceCloseSessions([item("x", "tab-x", false)], fx);
    expect(fx.forceClose).not.toHaveBeenCalled();
    expect(fx.dropTab.mock.calls).toEqual([["tab-x"]]);
    expect(results[0].state).toBe("closed");
  });
});

describe("forceCloseSessions", () => {
  it("kills only rows with a live tab and records them", async () => {
    const fx = effects({});
    fx.forceClose.mockResolvedValue({ kind: "killed" });
    const results = await forceCloseSessions([item("a"), item("b", null)], fx);
    expect(fx.forceClose.mock.calls).toEqual([["tab-a"]]);
    expect(fx.gracefulClose).not.toHaveBeenCalled();
    expect(results.map((r) => r.state)).toEqual(["closed", "not-closed"]);
    expect(fx.recordClosed.mock.calls[0][0]).toHaveLength(1);
  });

  it("reports what the close path observed, and records ONLY the confirmed kill", async () => {
    const fx = effects({});
    const byTab: Record<string, unknown> = {
      "tab-k": { kind: "killed" },
      "tab-w": { kind: "worker-view" },
      "tab-u": { kind: "unconfirmed", detail: "Terminal session not found" },
      "tab-v": undefined,
    };
    fx.forceClose.mockImplementation(async (tabId: string) => {
      if (tabId === "tab-t") throw new Error("ipc down");
      return byTab[tabId];
    });
    const results = await forceCloseSessions(
      ["k", "w", "u", "v", "t"].map((id) => item(id)),
      fx,
    );
    expect(results.map((r) => [r.sessionId, r.state])).toEqual([
      ["k", "closed"],
      ["w", "not-closed"],
      ["u", "unknown"],
      ["v", "unknown"],
      ["t", "unknown"],
    ]);
    expect(results[1].detail).toContain("a Conductor worker's tab is a view; nothing was killed");
    expect(results[2].detail).toBe("kill requested; not confirmed (Terminal session not found)");
    expect(results[3].detail).toContain("kill requested; not confirmed");
    expect(results[4].detail).toContain("ipc down");
    expect(fx.recordClosed.mock.calls).toEqual([
      [
        [
          {
            op: "recordClosed",
            sessionKey: "key-k",
            title: "Session k",
            closedAt: 1000,
            resumeCmd: "claude --resume k",
          },
        ],
      ],
    ]);
    expect(summarizeCloseResults(results)).toBe("1 of 5 closed · 1 not closed · 3 unknown");
  });

  it("records nothing when no kill was confirmed (a worker view hides, it does not close)", async () => {
    const fx = effects({});
    fx.forceClose.mockResolvedValue({ kind: "worker-view" });
    const results = await forceCloseSessions([item("w")], fx);
    expect(results[0].state).toBe("not-closed");
    expect(fx.recordClosed).not.toHaveBeenCalled();
  });
});

describe("describeForceCloseOutcome", () => {
  it("is closed only for an observed kill", () => {
    expect(describeForceCloseOutcome({ kind: "killed" }).state).toBe("closed");
    for (const o of [
      { kind: "worker-view" } as const,
      { kind: "no-process" } as const,
      { kind: "unconfirmed", detail: "x" } as const,
      undefined,
    ]) {
      const r = describeForceCloseOutcome(o);
      expect(r.state).not.toBe("closed");
      expect(r.detail).not.toMatch(/^(Force )?[Cc]losed/);
    }
  });
});

describe("graceful close ordering (CloseEffects.dropTab)", () => {
  it("drops a tab only after its graceful close answered closed", async () => {
    let resolveA: (v: unknown) => void = () => {};
    const fx = effects({});
    fx.gracefulClose.mockImplementation(
      (tabId: string) =>
        new Promise((resolve) => {
          if (tabId === "tab-a") resolveA = resolve;
          else resolve({ outcome: "refused", reason: "busy", claude_pids: [] });
        }),
    );
    const done = closeSessionsGracefully([item("a"), item("b")], fx);
    // Let every other microtask settle: tab-a's close has not answered yet.
    await new Promise((r) => setTimeout(r, 0));
    expect(fx.dropTab).not.toHaveBeenCalled();
    resolveA({ outcome: "exited", waited_ms: 10, claude_pids: [1] });
    await done;
    expect(fx.dropTab.mock.calls).toEqual([["tab-a"]]);
  });
});
