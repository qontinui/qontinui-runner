/**
 * The optimistic preview of organization ops, and the session-key spelling
 * shared with Rust. The preview must agree with `session_org_store.rs` on the
 * cases the Session Manager shows immediately, and must never show what the
 * runner would refuse.
 */

import { describe, it, expect } from "vitest";

import { applyOpsOptimistic, orderedMemberKeys } from "./sessionOrgOptimistic";
import { paneKeyString, sessionKeyFor, type SessionOrg } from "./sessionOrgTypes";

function emptyOrg(): SessionOrg {
  return {
    version: 1,
    revision: 0,
    groups: [],
    members: {},
    recentlyClosed: [],
    recentlyClosedTotal: 0,
    savedViews: [],
    dismissedSuggestions: [],
  };
}

describe("applyOpsOptimistic", () => {
  it("does not mutate its input", () => {
    const org = emptyOrg();
    applyOpsOptimistic(org, [{ op: "createGroup", groupId: "g", title: "G", sessionKeys: ["a"] }]);
    expect(org).toEqual(emptyOrg());
  });

  it("deleting a group ungroups its members after the existing ungrouped rows", () => {
    const org = applyOpsOptimistic(emptyOrg(), [
      { op: "setPinned", sessionKeys: ["loose"], pinned: true },
      { op: "createGroup", groupId: "g1", title: "A", sessionKeys: ["s1", "s2"] },
      { op: "addTags", sessionKeys: ["s2"], tags: ["keep"] },
      { op: "deleteGroup", groupId: "g1" },
    ]);
    expect(org.groups).toEqual([]);
    expect(orderedMemberKeys(org, null)).toEqual(["loose", "s1", "s2"]);
    expect(org.members.s2.tags).toEqual(["keep"]);
  });

  it("previews nothing the runner would refuse", () => {
    const org = applyOpsOptimistic(emptyOrg(), [
      { op: "createGroup", title: "no id yet" },
      { op: "createGroup", groupId: "g", title: "   " },
      { op: "addMembers", groupId: "missing", sessionKeys: ["s"] },
      { op: "setPinned", sessionKeys: ["ok", " "], pinned: true },
    ]);
    expect(org).toEqual(emptyOrg());
  });

  it("assigns the next unused color role and orders groups", () => {
    const org = applyOpsOptimistic(emptyOrg(), [
      { op: "createGroup", groupId: "a", title: "A", colorRole: "brand-primary" },
      { op: "createGroup", groupId: "b", title: "B" },
      { op: "moveGroup", groupId: "b", index: 0 },
    ]);
    expect(org.groups.find((g) => g.id === "b")?.colorRole).toBe("brand-secondary");
    expect([...org.groups].sort((x, y) => x.order - y.order).map((g) => g.id)).toEqual(["b", "a"]);
  });

  it("markSeen never moves backwards and recordClosed keeps the group", () => {
    const org = applyOpsOptimistic(emptyOrg(), [
      { op: "createGroup", groupId: "g", title: "G", sessionKeys: ["s"] },
      { op: "markSeen", sessionKey: "s", at: 10 },
      { op: "markSeen", sessionKey: "s", at: 5 },
      { op: "recordClosed", sessionKey: "s", title: "S", closedAt: 11 },
    ]);
    expect(org.members.s.lastSeenAt).toBe(10);
    expect(org.recentlyClosed[0]).toMatchObject({ sessionKey: "s", groupId: "g" });
    expect(org.recentlyClosedTotal).toBe(1);
  });
});

describe("sessionKeyFor", () => {
  it("prefers the Claude session id and spells the pane key exactly like Rust", () => {
    const pane = { pageId: "p", title: "Terminal 1", workingDir: "/w", windowLabel: "term-2" };
    expect(sessionKeyFor(" abc ", pane)).toBe("abc");
    // Pinned literal — `session_org_key_prefers_claude_id_and_pins_the_pane_spelling`
    // in session_org_store.rs asserts the same string.
    expect(sessionKeyFor(null, pane)).toBe("pane:p\u001fTerminal 1\u001f/w\u001fterm-2");
    expect(sessionKeyFor("  ", pane)).toBe(sessionKeyFor(undefined, pane));
    expect(
      sessionKeyFor(null, { pageId: "p", title: "T", workingDir: "/w", windowLabel: "main" }),
    ).toBe("pane:p\u001fT\u001f/w");
  });

  it("spells a remote pane like PaneKey::from_create_remote", () => {
    expect(
      paneKeyString({
        pageId: "p",
        title: "Terminal 1",
        workingDir: "/w",
        remoteDeviceId: " dev-a ",
      }),
    ).toBe("p\u001fTerminal 1\u001f/w\u001fremote\u001fdev-a");
  });
});
