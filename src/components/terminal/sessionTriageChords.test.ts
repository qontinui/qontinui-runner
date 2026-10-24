/**
 * `sessionTriageChords` — where Ctrl+Shift+N and Ctrl+Shift+↓ go (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 5).
 */

import { describe, expect, it } from "vitest";

import { nextInGroup, nextNeedsYouJump, type GroupPlace } from "./sessionTriageChords";
import type { SessionActivity } from "./sessionStatusModel";

const row = (sessionId: string, activity: SessionActivity = "idle-at-prompt") => ({
  sessionId,
  status: { activity },
});

describe("nextNeedsYouJump (Ctrl+Shift+N)", () => {
  const rows = [row("a"), row("b", "needs-you"), row("c"), row("d", "needs-you")];

  it("a tab here that needs input still wins", () => {
    expect(nextNeedsYouJump(["working", "needs-input"], rows, null)).toEqual({ kind: "tabs" });
  });

  it("with no tab needing input it falls through to the next Needs you row, cycling", () => {
    expect(nextNeedsYouJump(["working", undefined], rows, null)).toEqual({
      kind: "row",
      row: rows[1],
    });
    expect(nextNeedsYouJump([], rows, "b")).toEqual({ kind: "row", row: rows[3] });
    expect(nextNeedsYouJump([], rows, "d")).toEqual({ kind: "row", row: rows[1] });
  });

  it("with no row needing you either, the tab walk (errored fallback) runs", () => {
    expect(nextNeedsYouJump(["error"], [row("a"), row("c")], null)).toEqual({ kind: "tabs" });
  });
});

describe("nextInGroup (Ctrl+Shift+↓)", () => {
  const places: Record<string, GroupPlace | null> = {
    a: { groupId: "g", order: 2 },
    b: { groupId: "h", order: 0 },
    c: { groupId: "g", order: 0 },
    d: { groupId: "g", order: 1 },
    e: null,
  };
  const rows = ["a", "b", "c", "d", "e"].map((id) => row(id));
  const placeOf = (r: { sessionId: string }) => places[r.sessionId];
  const next = (from: string | null) => nextInGroup(rows, from, placeOf)?.sessionId ?? null;

  it("walks the group in its own order, not list order, and wraps", () => {
    expect(next("c")).toBe("d");
    expect(next("d")).toBe("a");
    expect(next("a")).toBe("c");
  });

  it("nothing for an ungrouped row, a lone member, an unknown row or no current row", () => {
    expect(next("e")).toBeNull();
    expect(next("b")).toBeNull();
    expect(next("missing")).toBeNull();
    expect(next(null)).toBeNull();
  });
});
