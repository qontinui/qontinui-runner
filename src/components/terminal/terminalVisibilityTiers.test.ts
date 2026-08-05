/**
 * terminalVisibilityTiers tests — the window-wide reconciler that tells the
 * runner what each terminal is worth (plan
 * `2026-07-28-runner-many-sessions-performance` Phase 5 / A4 backend half).
 *
 * The properties that matter:
 *  - a tab nobody declares is `unwatched` (the tier that costs ~0);
 *  - a mounted pane's declaration survives rapid visible/hidden toggling
 *    without churning IPC;
 *  - one layout change costs at most one call per tier, not one per tab;
 *  - a failed push is retried rather than assumed to have landed.
 */

import { describe, it, expect, vi, beforeEach } from "vitest";

const mockInvoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
}));

import {
  declarePaneTier,
  effectiveTier,
  publishRoster,
  releaseRoster,
  reconcileNow,
  __sentTiers,
  __resetVisibilityTiersForTest,
} from "./terminalVisibilityTiers";

/** The `terminal_set_visibility` calls made so far, as `[tier, ids]` pairs. */
function calls(): Array<[string, string[]]> {
  return mockInvoke.mock.calls
    .filter(([cmd]) => cmd === "terminal_set_visibility")
    .map(([, args]) => {
      const a = args as { ids: string[]; tier: string };
      return [a.tier, [...a.ids].sort()] as [string, string[]];
    });
}

beforeEach(() => {
  __resetVisibilityTiersForTest();
  mockInvoke.mockReset();
  mockInvoke.mockResolvedValue({ success: true });
});

describe("effectiveTier", () => {
  it("is unwatched for a tab nothing declares", () => {
    expect(effectiveTier("nobody")).toBe("unwatched");
  });

  it("takes the most visible live declaration", () => {
    const hidden = declarePaneTier("t1", "background");
    expect(effectiveTier("t1")).toBe("background");

    // A second pane (another zone, or the same tab mid zone-move) shows it.
    const shown = declarePaneTier("t1", "focused");
    expect(effectiveTier("t1")).toBe("focused");

    // Releasing the visible one falls back to the hidden one, not to unwatched.
    shown.release();
    expect(effectiveTier("t1")).toBe("background");

    hidden.release();
    expect(effectiveTier("t1")).toBe("unwatched");
  });

  it("updates a declaration in place when a pane toggles visibility", () => {
    const pane = declarePaneTier("t1", "focused");
    pane.update("background");
    expect(effectiveTier("t1")).toBe("background");
    pane.update("focused");
    expect(effectiveTier("t1")).toBe("focused");
  });

  it("ignores a double release", () => {
    const pane = declarePaneTier("t1", "focused");
    pane.release();
    pane.release();
    expect(effectiveTier("t1")).toBe("unwatched");
  });
});

describe("reconcile", () => {
  it("marks every rostered tab with no pane as unwatched, in ONE call", () => {
    publishRoster("page-1", ["a", "b", "c"]);
    reconcileNow();
    expect(calls()).toEqual([["unwatched", ["a", "b", "c"]]]);
  });

  it("groups a mixed layout into at most one call per tier", () => {
    publishRoster("page-1", ["a", "b", "c", "d"]);
    declarePaneTier("a", "focused");
    declarePaneTier("b", "background");
    declarePaneTier("c", "background");
    reconcileNow();

    const grouped = calls().sort((x, y) => x[0].localeCompare(y[0]));
    expect(grouped).toEqual([
      ["background", ["b", "c"]],
      ["focused", ["a"]],
      ["unwatched", ["d"]],
    ]);
  });

  it("sends only what CHANGED", () => {
    publishRoster("page-1", ["a", "b"]);
    const pane = declarePaneTier("a", "focused");
    reconcileNow();
    expect(calls()).toHaveLength(2);

    mockInvoke.mockClear();
    reconcileNow();
    expect(calls()).toEqual([]);

    pane.update("background");
    reconcileNow();
    expect(calls()).toEqual([["background", ["a"]]]);
  });

  it("collapses a burst of declarations into one reconcile", async () => {
    publishRoster("page-1", ["a", "b", "c"]);
    declarePaneTier("a", "focused");
    declarePaneTier("b", "focused");
    declarePaneTier("c", "focused");
    // Nothing has been pushed yet — the batch is still pending.
    expect(calls()).toEqual([]);
    await new Promise((r) => setTimeout(r, 0));
    expect(calls()).toEqual([["focused", ["a", "b", "c"]]]);
  });

  /**
   * Rapid toggling (a layout animation, a scroll that re-virtualizes a row)
   * must converge on the FINAL tier and cost one push, not one per flip.
   */
  it("survives rapid toggling with a single push of the final tier", async () => {
    publishRoster("page-1", ["a"]);
    const pane = declarePaneTier("a", "focused");
    for (let i = 0; i < 25; i++) {
      pane.update("background");
      pane.update("focused");
    }
    pane.update("background");
    await new Promise((r) => setTimeout(r, 0));
    expect(calls()).toEqual([["background", ["a"]]]);
  });

  it("drops a tab to unwatched when its last pane unmounts", () => {
    publishRoster("page-1", ["a"]);
    const pane = declarePaneTier("a", "focused");
    reconcileNow();
    expect(__sentTiers()).toEqual({ a: "focused" });

    mockInvoke.mockClear();
    pane.release();
    reconcileNow();
    expect(calls()).toEqual([["unwatched", ["a"]]]);
  });

  /**
   * A declaration alone cannot produce `unwatched` — only the absence of one
   * against a known roster can. A pane that mounts before its scope publishes
   * is still served, because the id enters the reconcile set from the
   * declaration side too.
   */
  it("serves a pane that declares before its scope publishes a roster", () => {
    declarePaneTier("early", "focused");
    reconcileNow();
    expect(calls()).toEqual([["focused", ["early"]]]);
  });

  it("forgets a tab once no roster owns it, so a rebind re-declares", () => {
    publishRoster("page-1", ["a"]);
    reconcileNow();
    expect(__sentTiers()).toEqual({ a: "unwatched" });

    releaseRoster("page-1");
    reconcileNow();
    expect(__sentTiers()).toEqual({});

    mockInvoke.mockClear();
    publishRoster("page-2", ["a"]);
    reconcileNow();
    expect(calls()).toEqual([["unwatched", ["a"]]]);
  });

  it("unions rosters across pages", () => {
    publishRoster("page-1", ["a"]);
    publishRoster("page-2", ["b"]);
    reconcileNow();
    expect(calls()).toEqual([["unwatched", ["a", "b"]]]);
  });

  it("ignores a roster republish with identical membership", () => {
    publishRoster("page-1", ["a", "b"]);
    reconcileNow();
    mockInvoke.mockClear();
    publishRoster("page-1", ["b", "a"]);
    reconcileNow();
    expect(calls()).toEqual([]);
  });

  /**
   * A dropped push must be retried. The runner defaults an unreported session
   * to `focused`, so a lost `unwatched` degrades to full service (safe) — but
   * believing it landed would strand the tab there forever.
   */
  it("retries after a failed push instead of assuming it landed", async () => {
    mockInvoke.mockRejectedValue(new Error("ipc down"));
    publishRoster("page-1", ["a"]);
    reconcileNow();
    await new Promise((r) => setTimeout(r, 0));
    expect(__sentTiers()).toEqual({});

    mockInvoke.mockReset();
    mockInvoke.mockResolvedValue({ success: true });
    reconcileNow();
    expect(calls()).toEqual([["unwatched", ["a"]]]);
  });
});
