/**
 * `sessionGrouping` — the Live list's sections for every lens, and the drag
 * op builders. Plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 3. Enumerated: 0 / 1 / 30 groups; a collapsed group with a `Needs
 * you` member still shows the count; a same-position drag sends nothing;
 * deleting the last member leaves the empty group visible.
 */

import { describe, expect, it } from "vitest";

import {
  DEFAULT_GROUP_BY,
  groupDropOp,
  groupSessions,
  memberDropOp,
  parseGroupBy,
  type GroupableSession,
  type GroupingOrg,
  type SessionSection,
} from "./sessionGrouping";
import { applyOpsOptimistic } from "./sessionOrgOptimistic";
import type { MemberOrg, SessionGroup, SessionOrg } from "./sessionOrgTypes";
import type { SessionActivity } from "./sessionStatusModel";

interface Row extends GroupableSession {
  title: string;
}

function row(
  id: string,
  activity: SessionActivity = "idle-at-prompt",
  over: Partial<Row> = {},
): Row {
  return {
    sessionId: id,
    title: id,
    accountLabel: "gmail",
    projectLabel: "runner",
    lastModified: "2026-10-13T10:00:00Z",
    status: { activity },
    ...over,
  };
}

function group(id: string, order: number, over: Partial<SessionGroup> = {}): SessionGroup {
  return {
    id,
    title: `Group ${id}`,
    colorRole: "brand-primary",
    collapsed: false,
    order,
    createdAt: 0,
    ...over,
  };
}

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

const known = (org: SessionOrg): GroupingOrg => ({ phase: "known", org });
const keyOf = (r: Row) => r.sessionId;

function sectionsFor(rows: Row[], org: GroupingOrg): SessionSection<Row>[] {
  return groupSessions({ sessions: rows, groupBy: "groups", org, keyOf });
}

const titles = (s: SessionSection<Row>) => s.sessions.map((r) => r.sessionId);

describe("the lens setting", () => {
  it("defaults to My groups and reads a retired value as the default", () => {
    expect(DEFAULT_GROUP_BY).toBe("groups");
    expect(parseGroupBy(null)).toBe("groups");
    expect(parseGroupBy("frozen")).toBe("groups");
    expect(parseGroupBy("status")).toBe("status");
  });
});

describe("My groups — cardinality", () => {
  it("0 groups: every row is Ungrouped, and Ungrouped is the only section", () => {
    const out = sectionsFor([row("a"), row("b")], known(orgOf([], {})));
    expect(out.map((s) => s.kind)).toEqual(["ungrouped"]);
    expect(titles(out[0])).toEqual(["a", "b"]);
  });

  it("0 groups and 0 rows: still an (empty) Ungrouped section, never nothing", () => {
    const out = sectionsFor([], known(orgOf([], {})));
    expect(out).toHaveLength(1);
    expect(out[0].kind).toBe("ungrouped");
    expect(out[0].sessions).toEqual([]);
  });

  it("1 group: its member under its header, the rest Ungrouped", () => {
    const org = orgOf([group("g", 0)], { a: member({ groupId: "g" }) });
    const out = sectionsFor([row("a"), row("b")], known(org));
    expect(out.map((s) => s.kind)).toEqual(["group", "ungrouped"]);
    expect(titles(out[0])).toEqual(["a"]);
    expect(titles(out[1])).toEqual(["b"]);
  });

  it("30 groups: every header in stored order, members in member order", () => {
    // Stored out of order on purpose: `order` decides, not array position.
    const groups = Array.from({ length: 30 }, (_, i) => group(`g${i}`, 29 - i));
    const members: Record<string, MemberOrg> = {};
    const rows: Row[] = [];
    for (let i = 0; i < 30; i++) {
      for (let j = 0; j < 2; j++) {
        const id = `s${i}-${j}`;
        // Reverse member order inside each group.
        members[id] = member({ groupId: `g${i}`, order: 1 - j });
        rows.push(row(id));
      }
    }
    const out = sectionsFor(rows, known(orgOf(groups, members)));
    const groupSections = out.filter((s) => s.kind === "group");
    expect(groupSections).toHaveLength(30);
    expect(groupSections.map((s) => s.kind === "group" && s.group.id)).toEqual(
      Array.from({ length: 30 }, (_, i) => `g${29 - i}`),
    );
    expect(titles(groupSections[0])).toEqual(["s29-1", "s29-0"]);
    expect(out[out.length - 1].kind).toBe("ungrouped");
    expect(out[out.length - 1].sessions).toEqual([]);
  });
});

describe("My groups — collapse, counts, pinned", () => {
  it("a collapsed group hides its rows but keeps its member and Needs-you counts", () => {
    const org = orgOf([group("g", 0, { collapsed: true })], {
      a: member({ groupId: "g", order: 0 }),
      b: member({ groupId: "g", order: 1 }),
      c: member({ groupId: "g", order: 2 }),
    });
    const out = sectionsFor(
      [row("a", "needs-you"), row("b", "working"), row("c", "needs-you")],
      known(org),
    );
    const g = out[0];
    expect(g.kind).toBe("group");
    if (g.kind !== "group") return;
    expect(g.collapsed).toBe(true);
    expect(g.sessions).toEqual([]);
    expect(g.shownCount).toBe(3);
    expect(g.memberCount).toBe(3);
    expect(g.needsYouCount).toBe(2);
  });

  it("memberCount counts stored members the list does not show (filtered or closed)", () => {
    const org = orgOf([group("g", 0)], {
      a: member({ groupId: "g" }),
      gone: member({ groupId: "g", order: 1 }),
    });
    const g = sectionsFor([row("a")], known(org))[0];
    expect(g.kind === "group" && [g.shownCount, g.memberCount]).toEqual([1, 2]);
  });

  it("pinned rows lead in a Pinned section and leave their group's list", () => {
    const org = orgOf([group("g", 0)], {
      a: member({ groupId: "g", pinned: true }),
      b: member({ pinned: true }),
      c: member({ groupId: "g", order: 1 }),
    });
    const out = sectionsFor([row("a"), row("b"), row("c"), row("d")], known(org));
    expect(out.map((s) => s.kind)).toEqual(["pinned", "group", "ungrouped"]);
    expect(titles(out[0])).toEqual(["a", "b"]);
    expect(titles(out[1])).toEqual(["c"]);
    expect(titles(out[2])).toEqual(["d"]);
  });

  it("deleting the LAST member leaves the empty group visible (Chrome keeps it)", () => {
    const before = orgOf([group("g", 0)], { a: member({ groupId: "g" }) });
    const after = applyOpsOptimistic(before, [{ op: "removeMembers", sessionKeys: ["a"] }]);
    const out = sectionsFor([row("a")], known(after));
    expect(out.map((s) => s.kind)).toEqual(["group", "ungrouped"]);
    const g = out[0];
    expect(g.kind === "group" && [g.group.id, g.shownCount, g.memberCount]).toEqual(["g", 0, 0]);
    expect(titles(out[1])).toEqual(["a"]);
  });

  it("a member naming a group the org no longer has reads as Ungrouped, not lost", () => {
    const out = sectionsFor([row("a")], known(orgOf([], { a: member({ groupId: "gone" }) })));
    expect(titles(out[0])).toEqual(["a"]);
  });
});

describe("My groups — org not known", () => {
  it.each([
    [{ phase: "loading" } as GroupingOrg, "Groups: loading"],
    [
      { phase: "unknown", reason: "file unreadable" } as GroupingOrg,
      "Groups unknown — file unreadable",
    ],
  ])("never renders %j as Ungrouped", (org, title) => {
    const out = sectionsFor([row("a")], org);
    expect(out).toHaveLength(1);
    expect(out[0].kind).toBe("org-unavailable");
    expect(out[0].title).toBe(title);
    expect(titles(out[0])).toEqual(["a"]);
  });
});

describe("the automatic lenses go through the same function", () => {
  const rows = [
    row("a", "needs-you", { accountLabel: "gmail", projectLabel: "runner" }),
    row("b", "working", { accountLabel: "hotmail", projectLabel: "" }),
    row("c", "needs-you", { accountLabel: "gmail", projectLabel: "coord" }),
  ];
  const lens = (groupBy: "status" | "account" | "project") =>
    groupSessions({ sessions: rows, groupBy, org: { phase: "loading" }, keyOf }).map((s) => [
      s.title,
      titles(s),
    ]);

  it("status: one section per activity word, first-seen order", () => {
    expect(lens("status")).toEqual([
      ["Needs you", ["a", "c"]],
      ["Working", ["b"]],
    ]);
  });

  it("account and project (an empty project reads Unknown)", () => {
    expect(lens("account")).toEqual([
      ["gmail", ["a", "c"]],
      ["hotmail", ["b"]],
    ]);
    expect(lens("project")).toEqual([
      ["runner", ["a"]],
      ["Unknown", ["b"]],
      ["coord", ["c"]],
    ]);
  });

  it("date: Today / Yesterday / This Week relative to `now`", () => {
    const now = new Date("2026-10-13T12:00:00");
    const dated = [
      row("t", "working", { lastModified: "2026-10-13T08:00:00" }),
      row("y", "working", { lastModified: "2026-10-12T08:00:00" }),
      row("w", "working", { lastModified: "2026-10-09T08:00:00" }),
      row("x", "working", { lastModified: "" }),
    ];
    expect(
      groupSessions({
        sessions: dated,
        groupBy: "date",
        org: { phase: "loading" },
        keyOf,
        now,
      }).map((s) => s.title),
    ).toEqual(["Today", "Yesterday", "This Week", "Unknown"]);
  });
});

describe("drag: member drops", () => {
  const org = orgOf([group("g", 0), group("h", 1)], {
    a: member({ groupId: "g", order: 0 }),
    b: member({ groupId: "g", order: 1 }),
    c: member({ groupId: "g", order: 2 }),
  });

  it("dropping a row back where it is sends nothing", () => {
    // Before its own successor = its current slot.
    expect(memberDropOp(org, "a", { groupId: "g", beforeKey: "b" })).toBeNull();
    // At the end of its group, when it already is last.
    expect(memberDropOp(org, "c", { groupId: "g", beforeKey: null })).toBeNull();
    // Onto itself.
    expect(memberDropOp(org, "b", { groupId: "g", beforeKey: "b" })).toBeNull();
    // Ungrouped onto Ungrouped.
    expect(memberDropOp(org, "z", { groupId: null, beforeKey: null })).toBeNull();
  });

  it("reorders within a group", () => {
    expect(memberDropOp(org, "c", { groupId: "g", beforeKey: "a" })).toEqual({
      op: "moveMember",
      sessionKey: "c",
      groupId: "g",
      index: 0,
    });
    const after = applyOpsOptimistic(org, [
      memberDropOp(org, "a", { groupId: "g", beforeKey: null })!,
    ]);
    expect(
      Object.entries(after.members)
        .filter(([, m]) => m.groupId === "g")
        .sort(([, x], [, y]) => x.order - y.order)
        .map(([k]) => k),
    ).toEqual(["b", "c", "a"]);
  });

  it("moves into another group, and out to Ungrouped", () => {
    expect(memberDropOp(org, "a", { groupId: "h", beforeKey: null })).toEqual({
      op: "moveMember",
      sessionKey: "a",
      groupId: "h",
      index: 0,
    });
    expect(memberDropOp(org, "z", { groupId: "g", beforeKey: "b" })).toEqual({
      op: "moveMember",
      sessionKey: "z",
      groupId: "g",
      index: 1,
    });
    expect(memberDropOp(org, "a", { groupId: null, beforeKey: null })).toEqual({
      op: "removeMembers",
      sessionKeys: ["a"],
    });
  });

  it("a drop into a group the org does not have sends nothing", () => {
    expect(memberDropOp(org, "a", { groupId: "gone", beforeKey: null })).toBeNull();
  });
});

describe("drag: group drops", () => {
  const org = orgOf([group("g", 0), group("h", 1), group("k", 2)], {});

  it("dropping a group where it already is sends nothing", () => {
    expect(groupDropOp(org, "h", "k")).toBeNull();
    expect(groupDropOp(org, "k", null)).toBeNull();
    expect(groupDropOp(org, "g", "g")).toBeNull();
    expect(groupDropOp(org, "gone", "g")).toBeNull();
  });

  it("moves a group before another, or last", () => {
    expect(groupDropOp(org, "k", "g")).toEqual({ op: "moveGroup", groupId: "k", index: 0 });
    expect(groupDropOp(org, "g", null)).toEqual({ op: "moveGroup", groupId: "g", index: 2 });
    const after = applyOpsOptimistic(org, [groupDropOp(org, "g", "k")!]);
    expect([...after.groups].sort((a, b) => a.order - b.order).map((g) => g.id)).toEqual([
      "h",
      "g",
      "k",
    ]);
  });
});
