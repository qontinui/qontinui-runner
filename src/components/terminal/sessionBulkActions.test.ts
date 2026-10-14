/**
 * Phase 5's gate file — plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`.
 *
 * Enumerated:
 *  - partial failure in a bulk close: one result per session (closed / not
 *    closed with the reason / unknown), never one "done" for the batch;
 *  - a session that changed to `Needs you` while the close dialog was open is
 *    re-checked at confirm and excluded (answered "not closed", the rest close);
 *  - a Shift-range across a collapsed group excludes its hidden members;
 *  - reopen after close restores group membership.
 * Plus the per-item contract of every other bulk action.
 */

import { describe, expect, it, vi } from "vitest";

import {
  bulkAddToGroup,
  bulkApplyOrg,
  bulkMarkFinished,
  bulkMute,
  bulkNewGroup,
  bulkPin,
  bulkResumeItems,
  bulkSnooze,
  closeTargetsFor,
  idsToClose,
  moveResults,
  resumeBlockerOf,
  summarizeBulkResults,
  type BulkItem,
} from "./sessionBulkActions";
import {
  closeSessionsGracefully,
  summarizeCloseResults,
  type CloseEffects,
  type CloseItem,
  type CloseTarget,
} from "./sessionClose";
import { groupSessions, type GroupableSession } from "./sessionGrouping";
import { applyOpsOptimistic } from "./sessionOrgOptimistic";
import type {
  MemberOrg,
  SessionGroup,
  SessionOrg,
  SessionOrgApplyResponse,
  SessionOrgOp,
} from "./sessionOrgTypes";
import { reopenClosedSession, type ReopenRow } from "./sessionReopen";
import { EMPTY_SELECTION, clickRow, renderedRowIds, selectAll } from "./sessionSelection";
import type { SessionActivity, SessionLocation } from "./sessionStatusModel";

// ── Fixtures ─────────────────────────────────────────────────────────────────

function member(over: Partial<MemberOrg> = {}): MemberOrg {
  return {
    groupId: null,
    order: 0,
    pinned: false,
    tags: [],
    muted: false,
    snoozedUntil: null,
    lastSeenAt: null,
    ...over,
  };
}

function group(id: string, order: number, over: Partial<SessionGroup> = {}): SessionGroup {
  return {
    id,
    title: `Group ${id}`,
    colorRole: "info",
    collapsed: false,
    order,
    createdAt: 0,
    ...over,
  };
}

function orgOf(groups: SessionGroup[], members: Record<string, MemberOrg>): SessionOrg {
  return {
    version: 1,
    revision: 1,
    groups,
    members,
    recentlyClosed: [],
    recentlyClosedTotal: 0,
    savedViews: [],
    dismissedSuggestions: [],
  };
}

/** An in-memory store that applies ops the way the runner's does. */
function memoryStore(initial: SessionOrg) {
  let org = initial;
  const apply = vi.fn(async (ops: SessionOrgOp[]): Promise<SessionOrgApplyResponse> => {
    org = applyOpsOptimistic(org, ops, 1_000);
    return {
      results: ops.map(() => ({ ok: true, error: null, groupId: null })),
      snapshot: { status: "known", org, reason: null, preservedAt: null },
    };
  });
  return { apply, get: () => org, set: (next: SessionOrg) => (org = next) };
}

function item(id: string, over: Partial<BulkItem> = {}): BulkItem {
  return {
    sessionId: id,
    title: `Session ${id}`,
    sessionKey: id,
    claudeSessionId: `claude-${id}`,
    resumeBlocker: null,
    ...over,
  };
}

function closeItem(id: string, over: Partial<CloseItem> = {}): CloseItem {
  return {
    sessionId: id,
    title: `Session ${id}`,
    tabId: `tab-${id}`,
    tabAlive: true,
    sessionKey: id,
    resumeCmd: `claude --resume ${id}`,
    ...over,
  };
}

function target(
  id: string,
  activity: SessionActivity,
  over: Partial<CloseTarget> = {},
): CloseTarget {
  const words: Partial<Record<SessionActivity, string>> = {
    "needs-you": "Needs you",
    working: "Working",
    unknown: "Unknown",
    "idle-at-prompt": "Idle",
    exited: "Exited",
  };
  return {
    sessionId: id,
    title: `Session ${id}`,
    activity,
    activityWord: words[activity] ?? activity,
    closable: true,
    ...over,
  };
}

function effects(over: Partial<CloseEffects> = {}): CloseEffects {
  return {
    gracefulClose: vi.fn(async () => ({ outcome: "exited", waited_ms: 1000, claude_pids: [1] })),
    forceClose: vi.fn(),
    dropTab: vi.fn(),
    recordClosed: vi.fn(),
    now: () => 5_000,
    ...over,
  };
}

// ── Partial failure in a bulk close ──────────────────────────────────────────

describe("a bulk close with partial failure", () => {
  const outcomes: Record<string, unknown> = {
    "tab-a": { outcome: "exited", waited_ms: 2000, claude_pids: [11] },
    "tab-b": { outcome: "refused", reason: "claude is mid-turn", claude_pids: [12] },
    "tab-c": { outcome: "probe_unavailable", detail: "ps failed" },
    "tab-e": { outcome: "close_outcome_unknown", waited_ms: 1000, claude_pids: [], detail: "x" },
  };
  const run = () => {
    const fx = effects({
      gracefulClose: vi.fn(async (tabId: string) => {
        if (tabId === "tab-d") throw new Error("no such terminal");
        return outcomes[tabId];
      }),
    });
    const items = ["a", "b", "c", "d", "e"].map((id) => closeItem(id));
    return { fx, promise: closeSessionsGracefully(items, fx) };
  };

  it("answers one result per session, in order, each with its own reason", async () => {
    const { promise } = run();
    const results = await promise;
    expect(results.map((r) => [r.sessionId, r.state])).toEqual([
      ["a", "closed"],
      ["b", "not-closed"],
      ["c", "not-closed"],
      ["d", "not-closed"],
      ["e", "unknown"],
    ]);
    expect(results[1].detail).toBe("not closed: claude is mid-turn");
    expect(results[2].detail).toContain("ps failed");
    expect(results[3].detail).toContain("no such terminal");
  });

  it("never reports a single success for the batch", async () => {
    const { promise } = run();
    const summary = summarizeCloseResults(await promise);
    expect(summary).toBe("1 of 5 closed · 3 not closed · 1 unknown");
    expect(summary).not.toMatch(/^(done|closed)$/i);
  });

  it("drops and records ONLY the session that actually closed — a refusal never kills", async () => {
    const { fx, promise } = run();
    await promise;
    expect(fx.forceClose).not.toHaveBeenCalled();
    expect(fx.dropTab).toHaveBeenCalledTimes(1);
    expect(fx.dropTab).toHaveBeenCalledWith("tab-a");
    expect(fx.recordClosed).toHaveBeenCalledTimes(1);
    expect(fx.recordClosed).toHaveBeenCalledWith([
      {
        op: "recordClosed",
        sessionKey: "a",
        title: "Session a",
        closedAt: 5_000,
        resumeCmd: "claude --resume a",
      },
    ]);
  });
});

// ── The re-check at confirm ──────────────────────────────────────────────────

describe("closeTargetsFor — a requested row that left the list", () => {
  it("stays as a non-closable placeholder, so the results answer N of N", async () => {
    // Asked for a, b, c; b left `allSessions` while the dialog was open.
    const rows = [{ sessionId: "a" }, { sessionId: "c" }];
    const targets = closeTargetsFor(["a", "b", "c"], rows, (r) =>
      target(r.sessionId, "idle-at-prompt"),
    );
    expect(targets.map((t) => [t.sessionId, t.closable])).toEqual([
      ["a", true],
      ["b", false],
      ["c", true],
    ]);
    const at = idsToClose(targets, new Set(), new Set());
    expect(at.ids).toEqual(["a", "c"]);
    expect(at.skipped).toEqual([
      {
        sessionId: "b",
        title: "b",
        state: "not-closed",
        detail: "not closed: it is no longer listed",
      },
    ]);
    const fx: CloseEffects = {
      gracefulClose: vi.fn(async () => ({ outcome: "exited", waited_ms: 1, claude_pids: [1] })),
      forceClose: vi.fn(),
      dropTab: vi.fn(),
      recordClosed: vi.fn(),
      now: () => 1,
    };
    const closed = await closeSessionsGracefully(
      at.ids.map((id) => closeItem(id)),
      fx,
    );
    expect(summarizeCloseResults([...closed, ...at.skipped])).toBe(
      "2 of 3 closed · 1 not closed",
    );
  });
});

describe("idsToClose — the re-check at confirm", () => {
  it("excludes a session that changed to Needs you while the dialog was open, and closes the rest", () => {
    // Opened while `b` read Idle; by the click it reads Needs you. No tick
    // covers Needs you, so it is left out and answered.
    const at = idsToClose(
      [target("a", "exited"), target("b", "needs-you"), target("c", "idle-at-prompt")],
      new Set(),
      new Set(),
    );
    expect(at.ids).toEqual(["a", "c"]);
    expect(at.needsTick.map((t) => t.sessionId)).toEqual(["b"]);
    expect(at.skipped).toEqual([
      {
        sessionId: "b",
        title: "Session b",
        state: "not-closed",
        detail: expect.stringContaining("changed to Needs you"),
      },
    ]);
  });

  it("a tick given for an earlier activity does not cover the new one", () => {
    // Ticked while it read Working; it now reads Needs you.
    const at = idsToClose([target("b", "needs-you")], new Set(), new Set(["b:working"]));
    expect(at.ids).toEqual([]);
    expect(at.needsTick).toHaveLength(1);
  });

  it("a tick for the current activity lets it close", () => {
    const at = idsToClose([target("b", "needs-you")], new Set(), new Set(["b:needs-you"]));
    expect(at).toEqual({ ids: ["b"], skipped: [], needsTick: [] });
  });

  it("Working and Unknown ask for the tick too; Idle and Exited do not", () => {
    const at = idsToClose(
      [
        target("w", "working"),
        target("u", "unknown"),
        target("i", "idle-at-prompt"),
        target("x", "exited"),
      ],
      new Set(),
      new Set(),
    );
    expect(at.ids).toEqual(["i", "x"]);
    expect(at.needsTick.map((t) => t.sessionId)).toEqual(["w", "u"]);
  });

  it("an operator-excluded row is neither closed nor reported; a row with no tab is reported", () => {
    const at = idsToClose(
      [target("a", "exited"), target("b", "needs-you"), target("c", "exited", { closable: false })],
      new Set(["b"]),
      new Set(),
    );
    expect(at.ids).toEqual(["a"]);
    expect(at.needsTick).toEqual([]);
    expect(at.skipped.map((s) => [s.sessionId, s.state])).toEqual([["c", "not-closed"]]);
  });
});

// ── Shift-range across a collapsed group ─────────────────────────────────────

interface Row extends GroupableSession {
  title: string;
}

function row(id: string): Row {
  return {
    sessionId: id,
    title: id,
    accountLabel: "gmail",
    projectLabel: "runner",
    lastModified: "2026-10-13T10:00:00Z",
    status: { activity: "idle-at-prompt" },
  };
}

describe("Shift-range across a collapsed group", () => {
  // Group g (expanded): g1, g2. Group h (COLLAPSED): h1, h2. Ungrouped: u1, u2.
  const org = orgOf([group("g", 0), group("h", 1, { collapsed: true })], {
    g1: member({ groupId: "g", order: 0 }),
    g2: member({ groupId: "g", order: 1 }),
    h1: member({ groupId: "h", order: 0 }),
    h2: member({ groupId: "h", order: 1 }),
  });
  const rows = ["g1", "g2", "h1", "h2", "u1", "u2"].map(row);
  const visible = renderedRowIds(
    groupSessions({
      sessions: rows,
      groupBy: "groups",
      org: { phase: "known", org },
      keyOf: (r) => r.sessionId,
    }),
  );

  it("the rendered order omits the collapsed group's members", () => {
    expect(visible).toEqual(["g1", "g2", "u1", "u2"]);
  });

  it("a range from above the collapsed group to below it excludes the hidden members", () => {
    const anchored = clickRow(EMPTY_SELECTION, visible, "g2", { toggle: false, range: false });
    const ranged = clickRow(anchored.state, visible, "u1", { toggle: false, range: true });
    expect([...ranged.state.ids]).toEqual(["g2", "u1"]);
    expect(ranged.state.ids.has("h1")).toBe(false);
    expect(ranged.state.ids.has("h2")).toBe(false);
    expect(ranged.open).toBeNull();
  });

  it("an upward range excludes them too, and Ctrl+A selects only what is shown", () => {
    const anchored = clickRow(EMPTY_SELECTION, visible, "u2", { toggle: false, range: false });
    const ranged = clickRow(anchored.state, visible, "g1", { toggle: false, range: true });
    expect([...ranged.state.ids].sort()).toEqual(["g1", "g2", "u1", "u2"]);
    expect([...selectAll(EMPTY_SELECTION, visible).ids]).toEqual(visible);
  });
});

// ── Reopen after close restores group membership ─────────────────────────────

interface LiveRow extends ReopenRow {
  title: string;
}

function liveRow(id: string, location: SessionLocation): LiveRow {
  return { sessionId: id, title: id, source: "transcript", status: { location } };
}

describe("reopen after close", () => {
  /** Close `a` (in group g) for real through the graceful path into the store. */
  async function closeIntoStore() {
    const store = memoryStore(
      orgOf([group("g", 0, { title: "Refactor" })], {
        a: member({ groupId: "g", order: 0 }),
        b: member({ groupId: "g", order: 1 }),
      }),
    );
    const fx = effects({ recordClosed: (ops) => void store.apply(ops) });
    await closeSessionsGracefully([closeItem("a")], fx);
    await Promise.resolve();
    return store;
  }

  it("records the group the session closed from", async () => {
    const store = await closeIntoStore();
    expect(store.get().recentlyClosed[0]).toMatchObject({ sessionKey: "a", groupId: "g" });
  });

  it("restores membership when the session left the group after closing", async () => {
    const store = await closeIntoStore();
    // The group lost the member meanwhile (dragged out, or the group was ungrouped).
    store.set(applyOpsOptimistic(store.get(), [{ op: "removeMembers", sessionKeys: ["a"] }]));
    expect(store.get().members.a?.groupId ?? null).toBeNull();

    const resume = vi.fn();
    const entry = store.get().recentlyClosed[0];
    const result = await reopenClosedSession(entry, [liveRow("a", "not-running")], store.get(), {
      resume,
      focus: vi.fn(),
      applyOrg: store.apply,
    });

    expect(resume).toHaveBeenCalledTimes(1);
    expect(store.apply).toHaveBeenLastCalledWith([
      { op: "addMembers", groupId: "g", sessionKeys: ["a"] },
    ]);
    expect(store.get().members.a?.groupId).toBe("g");
    expect(result).toEqual({ state: "done", detail: "Reopened in its group" });
  });

  it("a session still in its group reopens there with no op", async () => {
    const store = await closeIntoStore();
    const calls = store.apply.mock.calls.length;
    const result = await reopenClosedSession(
      store.get().recentlyClosed[0],
      [liveRow("a", "not-running")],
      store.get(),
      { resume: vi.fn(), focus: vi.fn(), applyOrg: store.apply },
    );
    expect(store.apply.mock.calls.length).toBe(calls);
    expect(store.get().members.a?.groupId).toBe("g");
    expect(result.state).toBe("done");
  });

  it("reads the restore BACK: an ok op that left it ungrouped is not done", async () => {
    const store = await closeIntoStore();
    store.set(applyOpsOptimistic(store.get(), [{ op: "removeMembers", sessionKeys: ["a"] }]));
    const lyingApply = vi.fn(
      async (): Promise<SessionOrgApplyResponse> => ({
        results: [{ ok: true, error: null, groupId: null }],
        snapshot: { status: "known", org: store.get(), reason: null, preservedAt: null },
      }),
    );
    const result = await reopenClosedSession(
      store.get().recentlyClosed[0],
      [liveRow("a", "not-running")],
      store.get(),
      { resume: vi.fn(), focus: vi.fn(), applyOrg: lyingApply },
    );
    expect(result.state).toBe("not-done");
  });

  it("an unreadable store after the restore is unknown, never done", async () => {
    const store = await closeIntoStore();
    store.set(applyOpsOptimistic(store.get(), [{ op: "removeMembers", sessionKeys: ["a"] }]));
    const result = await reopenClosedSession(
      store.get().recentlyClosed[0],
      [liveRow("a", "not-running")],
      store.get(),
      {
        resume: vi.fn(),
        focus: vi.fn(),
        applyOrg: async () => ({
          results: [{ ok: true, error: null, groupId: null }],
          snapshot: { status: "unknown", org: null, reason: "store unreadable", preservedAt: null },
        }),
      },
    );
    expect(result.state).toBe("unknown");
    expect(result.detail).toContain("store unreadable");
  });

  it("a resume that throws is not reopened, and no restore is attempted", async () => {
    const store = await closeIntoStore();
    const calls = store.apply.mock.calls.length;
    const result = await reopenClosedSession(
      store.get().recentlyClosed[0],
      [liveRow("a", "not-running")],
      store.get(),
      {
        resume: () => {
          throw new Error("spawn failed");
        },
        focus: vi.fn(),
        applyOrg: store.apply,
      },
    );
    expect(result).toEqual({ state: "not-done", detail: "not reopened: spawn failed" });
    expect(store.apply.mock.calls.length).toBe(calls);
  });
});

// ── The other bulk actions: one result per item ──────────────────────────────

describe("summarizeBulkResults", () => {
  it("states done of total and every shortfall", () => {
    expect(
      summarizeBulkResults({
        label: "Pinned",
        results: [
          { sessionId: "a", title: "a", state: "done", detail: "" },
          { sessionId: "b", title: "b", state: "not-done", detail: "" },
          { sessionId: "c", title: "c", state: "unknown", detail: "" },
        ],
      }),
    ).toBe("Pinned 1 of 3 · 1 not done · 1 unknown");
  });

  it("a full batch reads as all of them, still with the count", () => {
    expect(
      summarizeBulkResults({
        label: "Muted",
        results: [{ sessionId: "a", title: "a", state: "done", detail: "" }],
      }),
    ).toBe("Muted 1 of 1");
  });
});

describe("bulkMarkFinished", () => {
  it("answers each session — marked, refused, unknown, threw, no id", async () => {
    const setFinished = vi.fn(async (id: string) => {
      switch (id) {
        case "claude-a":
          return { success: true, data: { changed: "set", coord: { queued: true } } };
        case "claude-b":
          return { success: false, message: "unknown session" };
        case "claude-c":
          return "garbage";
        case "claude-d":
          throw new Error("ipc down");
        default:
          return {
            success: true,
            data: { changed: "none", coord: { queued: false, reason: "offline" } },
          };
      }
    });
    const results = await bulkMarkFinished(
      [item("a"), item("b"), item("c"), item("d"), item("e"), item("f", { claudeSessionId: null })],
      setFinished,
    );
    expect(results.map((r) => r.state)).toEqual([
      "done",
      "not-done",
      "unknown",
      "not-done",
      "done",
      "not-done",
    ]);
    expect(results[0].detail).toBe("Marked finished · coord told");
    expect(results[4].detail).toContain("local only, coord not told (offline)");
    expect(results[5].detail).toContain("no Claude session id");
    expect(setFinished).toHaveBeenCalledTimes(5);
  });
});

describe("bulkApplyOrg", () => {
  it("reads each item back: a session the store did not pin is not done", async () => {
    const apply = vi.fn(
      async (): Promise<SessionOrgApplyResponse> => ({
        results: [{ ok: true, error: null, groupId: null }],
        snapshot: {
          status: "known",
          org: orgOf([], { a: member({ pinned: true }) }),
          reason: null,
          preservedAt: null,
        },
      }),
    );
    const results = await bulkApplyOrg([item("a"), item("b")], bulkPin(true), apply);
    expect(apply).toHaveBeenCalledWith([
      { op: "setPinned", sessionKeys: ["a", "b"], pinned: true },
    ]);
    expect(results.map((r) => [r.sessionId, r.state, r.detail])).toEqual([
      ["a", "done", "Pinned"],
      ["b", "not-done", "not pinned"],
    ]);
  });

  it("a refused op names the refusal on each missed item", async () => {
    const results = await bulkApplyOrg([item("a")], bulkAddToGroup("g", "Refactor"), async () => ({
      results: [{ ok: false, error: "unknown_group", groupId: null }],
      snapshot: { status: "known", org: orgOf([], {}), reason: null, preservedAt: null },
    }));
    expect(results[0]).toMatchObject({
      state: "not-done",
      detail: "not in Refactor (refused: unknown_group)",
    });
  });

  it("a thrown apply or an unreadable snapshot is unknown for every item", async () => {
    const thrown = await bulkApplyOrg([item("a"), item("b")], bulkMute(true), async () => {
      throw new Error("ipc down");
    });
    expect(thrown.map((r) => r.state)).toEqual(["unknown", "unknown"]);
    const unread = await bulkApplyOrg([item("a")], bulkSnooze(10), async () => ({
      results: [{ ok: true, error: null, groupId: null }],
      snapshot: { status: "unknown", org: null, reason: "corrupt", preservedAt: null },
    }));
    expect(unread[0]).toMatchObject({
      state: "unknown",
      detail: expect.stringContaining("corrupt"),
    });
  });

  it("a new group is checked against the id the batch answered", async () => {
    const results = await bulkApplyOrg([item("a"), item("b")], bulkNewGroup("Fresh"), async () => ({
      results: [{ ok: true, error: null, groupId: "new-g" }],
      snapshot: {
        status: "known",
        org: orgOf([group("new-g", 0)], {
          a: member({ groupId: "new-g" }),
          b: member({ groupId: "old" }),
        }),
        reason: null,
        preservedAt: null,
      },
    }));
    expect(results.map((r) => r.state)).toEqual(["done", "not-done"]);
  });

  it("an empty selection sends nothing", async () => {
    const apply = vi.fn();
    expect(await bulkApplyOrg([], bulkPin(true), apply)).toEqual([]);
    expect(apply).not.toHaveBeenCalled();
  });
});

describe("moveResults", () => {
  it("maps each outcome and calls a missing one unknown", () => {
    const results = moveResults(
      [item("a"), item("b"), item("c")],
      [
        { sessionId: "a", ok: true, detail: "" },
        { sessionId: "b", ok: false, detail: "not moved: no tab" },
      ],
      "Work",
    );
    expect(results.map((r) => [r.state, r.detail])).toEqual([
      ["done", "Moved to Work"],
      ["not-done", "not moved: no tab"],
      ["unknown", "the move answered nothing for it"],
    ]);
  });
});

describe("resume", () => {
  it("only a transcript row known not to be running can be resumed", () => {
    const base = { source: "transcript" as const, injected: false };
    expect(resumeBlockerOf({ ...base, status: { location: "not-running" } })).toBeNull();
    expect(resumeBlockerOf({ ...base, status: { location: "unknown" } })).toContain("unknown");
    expect(resumeBlockerOf({ ...base, status: { location: "on-screen" } })).toContain("fork");
    expect(
      resumeBlockerOf({ ...base, source: "tab", status: { location: "not-running" } }),
    ).toContain("tab row");
    expect(
      resumeBlockerOf({ ...base, injected: true, status: { location: "not-running" } }),
    ).toContain("fixture");
  });

  it("launches each resumable item with a gap and reports the rest", async () => {
    const resume = vi.fn((i: BulkItem) => {
      if (i.sessionId === "c") throw new Error("no account");
    });
    const sleep = vi.fn(async () => {});
    const results = await bulkResumeItems(
      [item("a"), item("b", { resumeBlocker: "it is still running" }), item("c"), item("d")],
      { resume, sleep, gapMs: 10 },
    );
    expect(results.map((r) => r.state)).toEqual(["done", "not-done", "not-done", "done"]);
    expect(results[1].detail).toBe("not resumed: it is still running");
    expect(results[2].detail).toBe("not resumed: no account");
    expect(sleep).toHaveBeenCalledTimes(2);
  });
});
