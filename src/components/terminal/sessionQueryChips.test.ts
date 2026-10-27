import { describe, expect, it } from "vitest";

import { known, unknownFact } from "./sessionFacts";
import { NOW, facts } from "./sessionFacts.testkit";
import type { SessionGroup } from "./sessionOrgTypes";
import { filterSessions, parseQuery } from "./sessionQuery";
import {
  QUERY_CHIPS,
  SESSION_QUERY_CONTROLS,
  UNGROUPED_VALUE,
  applySavedView,
  chipActive,
  chipCounts,
  groupOptions,
  pickAccount,
  pickGroup,
  saveViewRefusal,
  savedViewActive,
  selectedAccount,
  selectedGroup,
  sessionQueryControlAttrs,
  sessionQueryControlId,
  slugify,
  toggleChip,
} from "./sessionQueryChips";

const ctx = { now: NOW };
const chip = (key: string) => {
  const c = QUERY_CHIPS.find((x) => x.key === key);
  if (!c) throw new Error(key);
  return c;
};

describe("chips", () => {
  it("lists the plan's chips, each a token the grammar parses", () => {
    expect(QUERY_CHIPS.map((c) => c.label)).toEqual([
      "Needs you",
      "Working",
      "Idle",
      "No output",
      "Finished",
      "Ready to close",
      "Outside runner",
      "Pinned",
    ]);
    for (const c of QUERY_CHIPS) expect(parseQuery(c.token).ok).toBe(true);
  });

  it("a click toggles the token, and typing the token lights the chip", () => {
    const needs = chip("needs-you");
    const q = toggleChip("repo:coord", needs);
    expect(q).toBe("repo:coord is:needs-you");
    expect(chipActive(q, needs)).toBe(true);
    expect(toggleChip(q, needs)).toBe("repo:coord");
    expect(chipActive("IS:Needs-You", needs)).toBe(true);
    expect(chipActive("-is:needs-you", needs)).toBe(false);
  });

  it("counts matches per chip and keeps undecidable rows apart", () => {
    const rows = [
      facts({ rowId: "a", activity: known("needs-you") }),
      facts({ rowId: "b", activity: known("needs-you"), pinned: known(true) }),
      facts({ rowId: "c", activity: known("working") }),
      facts({ rowId: "d", activity: unknownFact("no digest"), pinned: unknownFact("org loading") }),
    ];
    const counts = chipCounts(rows, ctx);
    expect(counts["needs-you"]).toEqual({ matched: 2, unknown: 1 });
    expect(counts.working).toEqual({ matched: 1, unknown: 1 });
    expect(counts.pinned).toEqual({ matched: 1, unknown: 1 });
    expect(counts["outside-runner"]).toEqual({ matched: 0, unknown: 0 });
  });

  it("a chip count equals what selecting the chip shows", () => {
    const rows = [
      facts({ rowId: "a", activity: known("exited"), exitCode: 0 }),
      facts({ rowId: "b", finished: known(true) }),
      facts({ rowId: "c" }),
    ];
    const counts = chipCounts(rows, ctx);
    const r = filterSessions(chip("ready-to-close").token, rows, ctx);
    expect(r.ok && r.matched.length).toBe(counts["ready-to-close"].matched);
    expect(counts["ready-to-close"].matched).toBe(2);
  });
});

describe("pickers", () => {
  it("the account picker sets, replaces and clears one account: term", () => {
    let q = pickAccount("is:working", "gmail");
    expect(q).toBe("is:working account:gmail");
    expect(selectedAccount(q)).toBe("gmail");
    q = pickAccount(q, "hotmail");
    expect(q).toBe("is:working account:hotmail");
    expect(pickAccount(q, null)).toBe("is:working");
    expect(selectedAccount("is:working")).toBeNull();
  });

  it("the group picker quotes a title with spaces and offers Ungrouped", () => {
    const q = pickGroup("", "Coord work");
    expect(q).toBe('group:"Coord work"');
    expect(selectedGroup(q)).toBe("Coord work");
    expect(pickGroup(q, UNGROUPED_VALUE)).toBe("group:none");
    const groups: SessionGroup[] = [
      { id: "2", title: "B", colorRole: "info", collapsed: false, order: 1, createdAt: 0 },
      { id: "1", title: "A", colorRole: "info", collapsed: false, order: 0, createdAt: 0 },
    ];
    expect(groupOptions(groups).map((o) => o.label)).toEqual(["A", "B", "Ungrouped"]);
  });

  it("a query joined by a top-level OR has no picker selection", () => {
    expect(selectedAccount("account:a OR account:b")).toBeNull();
  });
});

describe("saved views", () => {
  const view = { name: "Triage", query: "is:needs-you  repo:coord", savedAt: 0 };

  it("is lit when the box holds its query, whitespace aside", () => {
    expect(savedViewActive("is:needs-you repo:coord", view)).toBe(true);
    expect(savedViewActive("is:needs-you", view)).toBe(false);
  });

  it("clicking loads it; clicking it again clears the box", () => {
    expect(applySavedView("", view)).toBe(view.query);
    expect(applySavedView("is:needs-you repo:coord", view)).toBe("");
  });

  it("refuses to save an unnamed, empty or unparsable view", () => {
    expect(saveViewRefusal("", "is:working")).toBe("Name the view");
    expect(saveViewRefusal("x", "  ")).toBe("The query is empty");
    expect(saveViewRefusal("x", "bogus:1")).toBe("Fix the query before saving it");
    expect(saveViewRefusal("x", "is:working")).toBeNull();
  });
});

describe("control ids", () => {
  it("are terminal.session-org.* with a declared effect; writes are the view edits", () => {
    for (const c of Object.values(SESSION_QUERY_CONTROLS)) {
      expect(c.id.startsWith("terminal.session-org.")).toBe(true);
    }
    const writes = Object.entries(SESSION_QUERY_CONTROLS)
      .filter(([, c]) => c.effect !== "read")
      .map(([n]) => n)
      .sort();
    expect(writes).toEqual(["deleteView", "saveView"]);
    expect(sessionQueryControlAttrs("saveView")).toEqual({
      "data-ui-bridge-id": "terminal.session-org.query-save-view",
      "data-effect": "write",
    });
  });

  it("per-item ids are slugged", () => {
    expect(slugify("My View #1!")).toBe("my-view-1");
    expect(slugify("!!!")).toBe("x");
    expect(sessionQueryControlId("chip", "needs-you")).toBe(
      "terminal.session-org.query-chip-needs-you",
    );
  });
});
