/**
 * `sessionReopen` — what reopening a recently closed session does (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 5). The close → reopen round trip through the store is in the gate
 * file, `sessionBulkActions.test.ts`.
 */

import { describe, expect, it } from "vitest";

import type { ClosedSession, MemberOrg, SessionOrg } from "./sessionOrgTypes";
import { groupRestore, lastReopenable, planReopen, type ReopenRow } from "./sessionReopen";
import type { SessionLocation } from "./sessionStatusModel";

const member = (groupId: string | null): MemberOrg => ({
  groupId,
  order: 0,
  pinned: false,
  tags: [],
  muted: false,
  snoozedUntil: null,
  lastSeenAt: null,
});

const org = (members: Record<string, MemberOrg>, groupIds = ["g", "h"]): SessionOrg => ({
  version: 1,
  revision: 1,
  groups: groupIds.map((id, order) => ({
    id,
    title: `Group ${id}`,
    colorRole: "info",
    collapsed: false,
    order,
    createdAt: 0,
  })),
  members,
  recentlyClosed: [],
  recentlyClosedTotal: 0,
  savedViews: [],
  dismissedSuggestions: [],
});

const closed = (sessionKey: string, over: Partial<ClosedSession> = {}): ClosedSession => ({
  sessionKey,
  title: sessionKey,
  closedAt: 0,
  resumeCmd: `claude --resume ${sessionKey}`,
  groupId: "g",
  ...over,
});

const row = (
  sessionId: string,
  location: SessionLocation,
  source: "transcript" | "tab" = "transcript",
): ReopenRow => ({
  sessionId,
  source,
  status: { location },
});

describe("groupRestore", () => {
  it("adds it back to the group it closed from", () => {
    expect(groupRestore(closed("a"), org({}))).toEqual({
      ops: [{ op: "addMembers", groupId: "g", sessionKeys: ["a"] }],
      note: null,
    });
  });

  it("nothing to do when it is still there, or closed ungrouped", () => {
    expect(groupRestore(closed("a"), org({ a: member("g") }))).toEqual({ ops: [], note: null });
    expect(groupRestore(closed("a", { groupId: null }), org({}))).toEqual({ ops: [], note: null });
  });

  it("a group deleted since, or an unknown store, is said, not guessed", () => {
    expect(groupRestore(closed("a"), org({}, ["h"])).note).toContain("no longer exists");
    expect(groupRestore(closed("a"), null).note).toContain("unknown");
  });

  it("a later move to another group stands", () => {
    const r = groupRestore(closed("a"), org({ a: member("h") }));
    expect(r.ops).toEqual([]);
    expect(r.note).toContain("Group h");
  });
});

describe("planReopen", () => {
  it("resumes a transcript known not to be running, with its group restore", () => {
    const plan = planReopen(closed("a"), [row("a", "not-running")], org({}));
    expect(plan).toMatchObject({ kind: "resume", ops: [{ op: "addMembers" }] });
  });

  it("focuses one that is already live here — never forks it", () => {
    expect(planReopen(closed("a"), [row("a", "on-screen")], org({})).kind).toBe("focus");
    expect(planReopen(closed("a"), [row("a", "hidden-here")], org({})).kind).toBe("focus");
  });

  it("refuses one running elsewhere, one whose placement is unknown, one with no transcript", () => {
    expect(planReopen(closed("a"), [row("a", "unknown")], org({}))).toMatchObject({
      kind: "refused",
      reason: expect.stringContaining("unknown"),
    });
    expect(planReopen(closed("a"), [row("a", "outside-runner")], org({})).kind).toBe("refused");
    expect(
      planReopen(closed("a", { resumeCmd: null }), [row("a", "not-running")], org({})).kind,
    ).toBe("refused");
    expect(planReopen(closed("a"), [row("a", "not-running", "tab")], org({})).kind).toBe("refused");
    expect(planReopen(closed("a"), [], org({})).kind).toBe("refused");
  });
});

describe("lastReopenable (Ctrl+Shift+Backspace)", () => {
  it("takes the newest entry that would resume, passing over open and refused ones", () => {
    const entries = [
      closed("open"),
      closed("refused", { resumeCmd: null }),
      closed("b"),
      closed("c"),
    ];
    const rows = [
      row("open", "on-screen"),
      row("refused", "not-running"),
      row("b", "not-running"),
      row("c", "not-running"),
    ];
    expect(lastReopenable(entries, rows, org({}))?.entry.sessionKey).toBe("b");
  });

  it("null when nothing can be resumed", () => {
    expect(lastReopenable([closed("a")], [row("a", "on-screen")], org({}))).toBeNull();
    expect(lastReopenable([], [], org({}))).toBeNull();
  });
});
