/**
 * Tests for the terminal hot store (plan
 * `2026-07-28-runner-many-sessions-performance` Phase 1).
 *
 * Two properties carry the whole render-storm fix and are therefore asserted
 * directly rather than through a component:
 *
 *   1. **Snapshot stability.** `getField` / `getTabSlice` must return the same
 *      object identity between mutations. `useSyncExternalStore` re-invokes
 *      `getSnapshot` after every render and would loop forever otherwise.
 *   2. **Selector isolation.** A write for tab A must not notify tab B's
 *      listener — that is the difference between "one pane re-renders" and
 *      "the whole page re-renders 60×/s".
 *
 * Plus the no-change bailouts the 2 s / 10 s sweeps depend on.
 */

import { describe, it, expect, beforeEach } from "vitest";
import {
  TerminalHotStore,
  getTerminalHotStore,
  __resetTerminalHotStoresForTest,
} from "./terminalHotStore";
import type { LockState } from "./useFileLockTracking";

describe("TerminalHotStore — snapshot stability", () => {
  let store: TerminalHotStore;
  beforeEach(() => {
    store = new TerminalHotStore();
  });

  it("returns the identical field map until that field changes", () => {
    const first = store.getField("lastOutputLines");
    expect(store.getField("lastOutputLines")).toBe(first);

    store.setTabOutputLines("a", ["hello"]);
    const second = store.getField("lastOutputLines");
    expect(second).not.toBe(first);
    expect(store.getField("lastOutputLines")).toBe(second);
  });

  it("leaves other fields' identities untouched when one field changes", () => {
    const durations = store.getField("stateDurations");
    const locks = store.getField("lockStates");
    store.setTabOutputLines("a", ["hello"]);
    expect(store.getField("stateDurations")).toBe(durations);
    expect(store.getField("lockStates")).toBe(locks);
  });

  it("returns the identical tab slice until that tab changes", () => {
    store.setTabOutputLines("a", ["one"]);
    const slice = store.getTabSlice("a");
    expect(store.getTabSlice("a")).toBe(slice);

    // A write for a DIFFERENT tab must not invalidate this slice.
    store.setTabOutputLines("b", ["two"]);
    expect(store.getTabSlice("a")).toBe(slice);

    store.setTabOutputLines("a", ["three"]);
    expect(store.getTabSlice("a")).not.toBe(slice);
    expect(store.getTabSlice("a").lastOutputLines).toEqual(["three"]);
  });

  it("gives an unknown tab a stable empty slice", () => {
    const slice = store.getTabSlice("ghost");
    expect(slice.lastOutputLines).toEqual([]);
    expect(slice.activity).toEqual([]);
    expect(slice.stateDuration).toBeUndefined();
    expect(store.getTabSlice("ghost")).toBe(slice);
  });
});

describe("TerminalHotStore — per-tab selector isolation", () => {
  it("notifies only the changed tab's listeners", () => {
    const store = new TerminalHotStore();
    const calls: string[] = [];
    store.subscribeTab("a", () => calls.push("a"));
    store.subscribeTab("b", () => calls.push("b"));

    store.setTabOutputLines("a", ["x"]);
    expect(calls).toEqual(["a"]);

    store.setTabOutputLines("b", ["y"]);
    expect(calls).toEqual(["a", "b"]);
  });

  it("notifies field subscribers for any change in their field only", () => {
    const store = new TerminalHotStore();
    let lines = 0;
    let locks = 0;
    store.subscribeField("lastOutputLines", () => lines++);
    store.subscribeField("lockStates", () => locks++);

    store.setTabOutputLines("a", ["x"]);
    expect(lines).toBe(1);
    expect(locks).toBe(0);

    store.setField("lockStates", { a: { kind: "holding" } });
    expect(lines).toBe(1);
    expect(locks).toBe(1);
  });

  it("stops notifying after unsubscribe", () => {
    const store = new TerminalHotStore();
    let count = 0;
    const unsubscribe = store.subscribeTab("a", () => count++);
    store.setTabOutputLines("a", ["x"]);
    expect(count).toBe(1);
    unsubscribe();
    store.setTabOutputLines("a", ["y"]);
    expect(count).toBe(1);
  });

  it("notifies a multi-tab publish once per affected tab, and skips the rest", () => {
    const store = new TerminalHotStore();
    store.setField("stateDurations", { a: "1s", b: "1s", c: "1s" });
    const seen: string[] = [];
    store.subscribeTab("a", () => seen.push("a"));
    store.subscribeTab("b", () => seen.push("b"));
    store.subscribeTab("c", () => seen.push("c"));

    // Only `b` moved; `a` and `c` reuse their published strings.
    store.setField("stateDurations", { a: "1s", b: "2s", c: "1s" });
    expect(seen).toEqual(["b"]);
  });
});

describe("TerminalHotStore — no-change bailouts", () => {
  it("publishes nothing when every entry is reference-identical", () => {
    const store = new TerminalHotStore();
    const buf = [1, 2, 3];
    expect(store.setField("activityData", { a: buf })).toBe(true);

    let notified = 0;
    store.subscribeField("activityData", () => notified++);
    const before = store.getField("activityData");

    // The sparkline sweep reuses the published array when the values are
    // unchanged — the store must then bail entirely.
    expect(store.setField("activityData", { a: buf })).toBe(false);
    expect(notified).toBe(0);
    expect(store.getField("activityData")).toBe(before);
  });

  it("publishes when a key is added or removed even if the rest match", () => {
    const store = new TerminalHotStore();
    const held: LockState = { kind: "holding", filePath: "a.rs" };
    store.setField("lockStates", { a: held });

    expect(store.setField("lockStates", { a: held, b: { kind: "idle" } })).toBe(true);
    expect(store.setField("lockStates", { a: held })).toBe(true);
  });

  it("treats an updater that returns the same object as a no-op", () => {
    const store = new TerminalHotStore();
    let notified = 0;
    store.subscribeField("pendingYieldRequests", () => notified++);
    expect(store.updateField("pendingYieldRequests", (prev) => prev)).toBe(false);
    expect(notified).toBe(0);
  });

  it("ignores a repeated write of the identical lines array", () => {
    const store = new TerminalHotStore();
    const lines = ["a"];
    store.setTabOutputLines("a", lines);
    let notified = 0;
    store.subscribeTab("a", () => notified++);
    store.setTabOutputLines("a", lines);
    expect(notified).toBe(0);
  });
});

describe("TerminalHotStore — lifecycle", () => {
  it("retainTabs drops dead tabs across every field and notifies them", () => {
    const store = new TerminalHotStore();
    store.setTabOutputLines("alive", ["x"]);
    store.setTabOutputLines("dead", ["y"]);
    store.setField("stateDurations", { alive: "1s", dead: "2s" });

    let deadNotified = 0;
    store.subscribeTab("dead", () => deadNotified++);

    store.retainTabs(new Set(["alive"]));

    expect(store.getField("lastOutputLines")).toEqual({ alive: ["x"] });
    expect(store.getField("stateDurations")).toEqual({ alive: "1s" });
    // One notification per field the tab appeared in.
    expect(deadNotified).toBe(2);
    expect(store.getTabSlice("dead").lastOutputLines).toEqual([]);
  });

  it("retainTabs is a no-op when every tab is still live", () => {
    const store = new TerminalHotStore();
    store.setTabOutputLines("a", ["x"]);
    const before = store.getField("lastOutputLines");
    let notified = 0;
    store.subscribeField("lastOutputLines", () => notified++);
    store.retainTabs(new Set(["a"]));
    expect(store.getField("lastOutputLines")).toBe(before);
    expect(notified).toBe(0);
  });

  it("reset clears every field and notifies affected listeners once", () => {
    const store = new TerminalHotStore();
    store.setTabOutputLines("a", ["x"]);
    let fieldNotified = 0;
    let tabNotified = 0;
    store.subscribeField("lastOutputLines", () => fieldNotified++);
    store.subscribeTab("a", () => tabNotified++);

    store.reset();
    expect(store.getField("lastOutputLines")).toEqual({});
    expect(fieldNotified).toBe(1);
    expect(tabNotified).toBe(1);

    // Second reset has nothing to do.
    store.reset();
    expect(fieldNotified).toBe(1);
  });
});

describe("getTerminalHotStore — per-page scoping", () => {
  beforeEach(() => {
    __resetTerminalHotStoresForTest();
  });

  it("returns the same instance for a page id and distinct ones per page", () => {
    const a1 = getTerminalHotStore("page-a");
    const a2 = getTerminalHotStore("page-a");
    const b = getTerminalHotStore("page-b");
    expect(a1).toBe(a2);
    expect(b).not.toBe(a1);
  });

  it("keeps pages' data disjoint", () => {
    getTerminalHotStore("page-a").setTabOutputLines("t1", ["from a"]);
    expect(getTerminalHotStore("page-b").getField("lastOutputLines")).toEqual({});
    expect(getTerminalHotStore("page-a").getTabSlice("t1").lastOutputLines).toEqual(["from a"]);
  });
});
