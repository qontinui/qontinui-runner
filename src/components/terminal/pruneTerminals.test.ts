/**
 * `planPrune` — which windows the "Keep AI sessions" button closes, the order
 * the survivors compact into, and the layout they land in.
 *
 * vitest runs `environment: "node"` here — pure-function tests only.
 */

import { describe, it, expect } from "vitest";
import { hasAiSession, isPruneActionable, planPrune, RECENT_TAB_GRACE_MS } from "./pruneTerminals";
import type { RemoteTabIdentity } from "./remoteTabs";
import { compactAssignments, resolveLayout } from "./useZoneLayout";

const ai = (id: string) => ({ id, title: id, claudeSessionId: `sid-${id}` });
const shell = (id: string) => ({ id, title: id });

describe("hasAiSession", () => {
  it("keeps provider sessions, Conductor workers and remote mirrors", () => {
    expect(hasAiSession(ai("a"))).toBe(true);
    expect(hasAiSession({ id: "w", title: "w", sessionBacked: true })).toBe(true);
    expect(hasAiSession({ id: "w", title: "w", taskRunId: "run-1" })).toBe(true);
    expect(hasAiSession({ id: "r", title: "r", remote: {} as RemoteTabIdentity })).toBe(true);
  });

  it("closes a plain shell", () => {
    expect(hasAiSession(shell("s"))).toBe(false);
  });

  it("keeps a shell the detector reads as working or awaiting input (id not bound yet)", () => {
    expect(hasAiSession(shell("s"), { sessionStates: { s: "working" } })).toBe(true);
    expect(hasAiSession(shell("s"), { sessionStates: { s: "needs-input" } })).toBe(true);
    expect(hasAiSession(shell("s"), { sessionStates: { s: "idle" } })).toBe(false);
  });

  it("keeps a tab inside the launch grace window, closes it after", () => {
    const now = 1_000_000;
    const young = { ...shell("s"), createdAt: now - RECENT_TAB_GRACE_MS + 1 };
    const old = { ...shell("s"), createdAt: now - RECENT_TAB_GRACE_MS };
    expect(hasAiSession(young, { now })).toBe(true);
    expect(hasAiSession(old, { now })).toBe(false);
  });
});

describe("planPrune", () => {
  it("the 9-zone example: 3 AI + 3 shells + 3 empty → quad with the 3 AI in zones 0-2", () => {
    const tabs = [ai("a1"), shell("s1"), ai("a2"), shell("s2"), ai("a3"), shell("s3")];
    // full-grid: a1@1, s1@0, a2@4, s2@5, a3@8, s3@3; zones 2, 6, 7 empty.
    const assignments = { 0: "s1", 1: "a1", 3: "s3", 4: "a2", 5: "s2", 8: "a3" };
    const plan = planPrune(tabs, assignments);

    expect(plan.closeIds).toEqual(["s1", "s2", "s3"]);
    expect(plan.closeTitles).toEqual(["s1", "s2", "s3"]);
    expect(plan.keepIds).toEqual(["a1", "a2", "a3"]);
    expect(plan.layoutId).toBe("quad");
    expect(resolveLayout(plan.layoutId, 3).zones.length).toBe(4);
    expect(compactAssignments(plan.keepIds)).toEqual({ 0: "a1", 1: "a2", 2: "a3" });
  });

  it("orders kept tabs by zone, then appends hidden AI tabs in tab order", () => {
    const tabs = [ai("hidden1"), ai("z5"), ai("z2"), ai("hidden2")];
    const plan = planPrune(tabs, { 2: "z2", 5: "z5" });
    expect(plan.keepIds).toEqual(["z2", "z5", "hidden1", "hidden2"]);
    expect(plan.layoutId).toBe("quad");
  });

  it("no AI sessions → everything closes, single empty zone", () => {
    const plan = planPrune([shell("s1"), shell("s2")], { 0: "s1", 1: "s2" });
    expect(plan.closeIds).toEqual(["s1", "s2"]);
    expect(plan.keepIds).toEqual([]);
    expect(plan.layoutId).toBe("single");
  });

  it("ignores synthetic fixture tabs", () => {
    const plan = planPrune([{ id: "fx", title: "fx", __synthetic: true }, ai("a")], { 0: "a" });
    expect(plan.closeIds).toEqual([]);
    expect(plan.keepIds).toEqual(["a"]);
  });

  it("past nine AI sessions compacts into the flow grid", () => {
    const tabs = Array.from({ length: 11 }, (_, i) => ai(`a${i}`));
    expect(planPrune(tabs, {}).layoutId).toBe("flow-grid");
  });
});

describe("isPruneActionable", () => {
  it("is actionable when there is something to close", () => {
    const plan = planPrune([ai("a"), shell("s")], { 0: "a", 1: "s" });
    expect(isPruneActionable(plan, 2, 1, { 0: "a", 1: "s" })).toBe(true);
  });

  it("is actionable when only the grid is oversized", () => {
    const plan = planPrune([ai("a")], { 0: "a" });
    expect(isPruneActionable(plan, 9, 1, { 0: "a" })).toBe(true);
  });

  it("is actionable when the survivors are not a dense prefix", () => {
    const plan = planPrune([ai("a"), ai("b")], { 0: "a", 3: "b" });
    expect(isPruneActionable(plan, 2, 2, { 0: "a", 3: "b" })).toBe(true);
  });

  it("is NOT actionable on an already-compact AI-only grid", () => {
    const plan = planPrune([ai("a"), ai("b")], { 0: "a", 1: "b" });
    expect(isPruneActionable(plan, 2, 2, { 0: "a", 1: "b" })).toBe(false);
  });
});
