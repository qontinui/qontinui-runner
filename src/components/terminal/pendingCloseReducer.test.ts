/**
 * The deferred-kill close — BEHAVIOUR, not markup.
 *
 * Closing a zone removes the tab at once and holds the PTY kill back for the
 * Undo window. These tests pin the state transitions `useTerminalManager`
 * drives through `pendingCloseReducer.ts` (the runner's vitest environment is
 * `node` with no React Testing Library, so the hook itself cannot be driven —
 * same shape as `hiddenWorkerReducer.test.ts`).
 */

import { describe, it, expect } from "vitest";
import {
  EMPTY_PENDING_CLOSE_STATE,
  PENDING_CLOSE_WINDOW_MS,
  beginPendingClose,
  excludePendingCloses,
  expirePendingClose,
  flushPendingCloses,
  pendingCloseMessage,
  restorePendingTab,
  routeClose,
  undoPendingClose,
  type PendingClose,
  type PendingCloseState,
} from "./pendingCloseReducer";
import { reconcileTabsWithBackend, type TerminalTab } from "./useTerminalManager";

const tab = (id: string, partial: Partial<TerminalTab> = {}): TerminalTab => ({
  id,
  title: `Terminal ${id}`,
  pid: 100,
  isAlive: true,
  exitCode: null,
  createdAt: 1_000,
  ...partial,
});

const entry = (t: TerminalTab, partial: Partial<PendingClose> = {}): PendingClose => ({
  tab: t,
  index: 1,
  wasActive: false,
  closedAtMs: 5_000,
  ...partial,
});

/** Close `t` the way the hook does for a single-target user close. */
function close(state: PendingCloseState, t: TerminalTab, index = 1): PendingCloseState {
  expect(routeClose(state, t.id, t, false)).toBe("defer");
  return beginPendingClose(state, entry(t, { index }));
}

describe("the Undo window", () => {
  it("is the action toast's lifetime, 10 s", () => {
    expect(PENDING_CLOSE_WINDOW_MS).toBe(10_000);
  });
});

describe("close → pending", () => {
  it("records the closed tab as pending, keyed by its id", () => {
    const b = tab("b");
    const state = close(EMPTY_PENDING_CLOSE_STATE, b);
    expect([...state.pending.keys()]).toEqual(["b"]);
    expect(state.pending.get("b")?.tab).toBe(b);
    expect(state.pending.get("b")?.index).toBe(1);
  });

  it("a second close of the same id keeps the one entry (no second timer/toast)", () => {
    const b = tab("b");
    const once = close(EMPTY_PENDING_CLOSE_STATE, b);
    expect(routeClose(once, "b", undefined, false)).toBe("already-pending");
    const twice = beginPendingClose(once, entry(b, { index: 7 }));
    expect(twice).toBe(once);
    expect(twice.pending.get("b")?.index).toBe(1);
  });

  it("two different tabs pend independently", () => {
    const s = close(close(EMPTY_PENDING_CLOSE_STATE, tab("a"), 0), tab("b"), 1);
    expect([...s.pending.keys()]).toEqual(["a", "b"]);
    const { state: afterUndoA, restored } = undoPendingClose(s, "a");
    expect(restored?.tab.id).toBe("a");
    expect([...afterUndoA.pending.keys()]).toEqual(["b"]);
  });
});

describe("undo → restored, and the kill never fires", () => {
  it("returns the entry, empties the set, and a later expiry kills nothing", () => {
    const s = close(EMPTY_PENDING_CLOSE_STATE, tab("b"));
    const undone = undoPendingClose(s, "b");
    expect(undone.restored?.tab.id).toBe("b");
    expect(undone.state.pending.size).toBe(0);
    const expired = expirePendingClose(undone.state, "b");
    expect(expired.kill).toBeNull();
  });

  it("puts the tab back at the position it had", () => {
    const tabs = [tab("a"), tab("c")];
    const restored = restorePendingTab(tabs, entry(tab("b"), { index: 1 }));
    expect(restored.map((t) => t.id)).toEqual(["a", "b", "c"]);
  });

  it("clamps a stale position to the end of a shorter list", () => {
    const restored = restorePendingTab([tab("a")], entry(tab("b"), { index: 5 }));
    expect(restored.map((t) => t.id)).toEqual(["a", "b"]);
  });

  it("restoring a tab already present changes nothing", () => {
    const tabs = [tab("a"), tab("b")];
    expect(restorePendingTab(tabs, entry(tab("b")))).toBe(tabs);
  });

  it("undo after expiry is a no-op", () => {
    const s = close(EMPTY_PENDING_CLOSE_STATE, tab("b"));
    const expired = expirePendingClose(s, "b");
    const undone = undoPendingClose(expired.state, "b");
    expect(undone.restored).toBeNull();
    expect(undone.state).toBe(expired.state);
  });
});

describe("expiry → killed exactly once", () => {
  it("the first expiry yields the kill, the second yields nothing", () => {
    const s = close(EMPTY_PENDING_CLOSE_STATE, tab("b", { claudeSessionId: "sess-1" }));
    const first = expirePendingClose(s, "b");
    expect(first.kill?.tab.id).toBe("b");
    expect(first.kill?.tab.claudeSessionId).toBe("sess-1");
    expect(first.state.pending.size).toBe(0);
    const second = expirePendingClose(first.state, "b");
    expect(second.kill).toBeNull();
  });

  it("an immediate close of a pending tab expedites it instead of re-closing", () => {
    const s = close(EMPTY_PENDING_CLOSE_STATE, tab("b"));
    expect(routeClose(s, "b", undefined, true)).toBe("expedite-pending");
  });

  it("unmount flushes every pending close into kills and leaves none pending", () => {
    const s = close(close(EMPTY_PENDING_CLOSE_STATE, tab("a"), 0), tab("b"), 1);
    const flushed = flushPendingCloses(s);
    expect(flushed.kills.map((k) => k.tab.id)).toEqual(["a", "b"]);
    expect(flushed.state.pending.size).toBe(0);
    expect(flushPendingCloses(flushed.state).kills).toEqual([]);
  });
});

describe("resync while pending does not resurrect", () => {
  const backend = [
    { id: "a", title: "Terminal a", pid: 1, isAlive: true, createdAt: 1_000 },
    { id: "b", title: "Terminal b", pid: 2, isAlive: true, createdAt: 1_000 },
  ] as Parameters<typeof reconcileTabsWithBackend>[1];

  it("drops the pending id from the backend list before the reconcile", () => {
    const s = close(EMPTY_PENDING_CLOSE_STATE, tab("b"));
    expect(excludePendingCloses(backend, s).map((t) => t.id)).toEqual(["a"]);
  });

  it("so the reconcile does not re-add the closed tab", () => {
    const s = close(EMPTY_PENDING_CLOSE_STATE, tab("b"));
    const local = [tab("a")];
    const next = reconcileTabsWithBackend(local, excludePendingCloses(backend, s), 60_000);
    expect(next.map((t) => t.id)).toEqual(["a"]);
  });

  it("with nothing pending the backend list passes through unchanged", () => {
    expect(excludePendingCloses(backend, EMPTY_PENDING_CLOSE_STATE)).toBe(backend);
  });
});

describe("routeClose", () => {
  const empty = EMPTY_PENDING_CLOSE_STATE;
  it.each([
    ["a single-target user close defers", "t1", tab("t1"), false, "defer"],
    ["an opted-out (immediate) close kills immediately", "t1", tab("t1"), true, "kill-now"],
    ["an id with no tab to snapshot kills immediately", "ghost", undefined, false, "kill-now"],
    [
      "a Conductor worker view is hidden, never killed",
      "w1",
      tab("w1", { sessionBacked: true, taskRunId: "w1" }),
      false,
      "hide-worker",
    ],
    [
      "a plan tab is just removed (no PTY)",
      "plan-1",
      tab("plan-1", { type: "plan" }),
      false,
      "remove-view",
    ],
  ] as const)("%s", (_label, id, t, immediate, expected) => {
    expect(routeClose(empty, id, t, immediate)).toBe(expected);
  });
});

describe("pendingCloseMessage", () => {
  it("names the session by its spawn name, else its title", () => {
    expect(pendingCloseMessage(tab("a", { spawnName: "fix-parser" }))).toBe("Closed fix-parser");
    expect(pendingCloseMessage(tab("a"))).toBe("Closed Terminal a");
  });
});
