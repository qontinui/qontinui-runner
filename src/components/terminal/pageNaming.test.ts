/**
 * The one page-name rule (plan 2026-11-05 Phase 5, D1): pure helpers, and
 * `reconcilePages` / `resolveProjectPage` naming over the roster measured on
 * 2026-11-05 (`fixtures/terminalPageRoster.ts`).
 */

import { describe, expect, it } from "vitest";

import { nextAutoPageName, renameRefusal, uniquePageName } from "./pageNaming";
import {
  LATER_PAGE_IDS,
  RESTART_BATCH_PAGE_IDS,
  ROSTER_2026_11_05,
  ROSTER_BEFORE_RESTART,
} from "./fixtures/terminalPageRoster";
import { reconcilePages, resolveProjectPage, type TerminalPageConfig } from "./useTerminalPages";

const named = (...names: string[]) => names.map((name) => ({ name }));

/** Names that occur more than once among the pages whose ids are not in `legacy`. */
function newCollisions(pages: readonly TerminalPageConfig[], legacy: ReadonlySet<string>) {
  const out: string[] = [];
  for (const p of pages) {
    if (legacy.has(p.id)) continue;
    if (pages.some((q) => q.id !== p.id && q.name === p.name)) out.push(p.name);
  }
  return out;
}

describe("nextAutoPageName", () => {
  it('over ["Page 2", "Page 5"] is "Page 6"', () => {
    expect(nextAutoPageName(named("Page 2", "Page 5"))).toBe("Page 6");
  });

  it('is "Page 1" when no page has an auto name', () => {
    expect(nextAutoPageName(named("Terminal", "tmux", "Page 3b", "page 4"))).toBe("Page 1");
    expect(nextAutoPageName([])).toBe("Page 1");
  });

  it("is one past the max over the measured 40-page roster", () => {
    expect(nextAutoPageName(ROSTER_2026_11_05)).toBe("Page 40");
  });
});

describe("uniquePageName", () => {
  it("keeps a free name", () => {
    expect(uniquePageName("tmux", named("Terminal", "Page 1"))).toBe("tmux");
  });

  it("suffixes a used name with the first free number", () => {
    expect(uniquePageName("tmux", named("tmux"))).toBe("tmux 2");
    expect(uniquePageName("tmux", named("tmux", "tmux 2"))).toBe("tmux 3");
  });

  it('never yields "Terminal" — that name is the default page\'s', () => {
    expect(uniquePageName("Terminal", named("Terminal", "Page 4"))).toBe("Page 5");
    expect(uniquePageName("Terminal", [])).toBe("Page 1");
  });
});

describe("renameRefusal", () => {
  const pages = [
    { id: "default", name: "Terminal" },
    { id: "a", name: "tmux" },
    { id: "b", name: "Page 2" },
  ];

  it("refuses a name another page uses", () => {
    expect(renameRefusal(pages, "b", "tmux")).toBe("Name in use");
  });

  it('refuses "Terminal" for a non-default page', () => {
    expect(renameRefusal(pages, "b", "Terminal")).toBe("Name in use");
  });

  it("allows a free name, a page's own name, and the default page keeping its own", () => {
    expect(renameRefusal(pages, "b", "agentic")).toBeNull();
    expect(renameRefusal(pages, "a", "tmux")).toBeNull();
    expect(renameRefusal(pages, "default", "Terminal")).toBeNull();
  });
});

describe("reconcilePages over the measured 2026-11-05 roster", () => {
  it("names the 20:51Z restart batch without taking a name already present", () => {
    // The old count rule named this batch "Page 19" … "Page 27" beside the
    // October "Page 20" … "Page 23" — the measured duplicates.
    const before = new Set(ROSTER_BEFORE_RESTART.map((p) => p.id));
    const out = reconcilePages([...ROSTER_BEFORE_RESTART], RESTART_BATCH_PAGE_IDS);
    const batch = out.filter((p) => !before.has(p.id));
    expect(batch.map((p) => p.name)).toEqual([
      "Page 24",
      "Page 25",
      "Page 26",
      "Page 27",
      "Page 28",
      "Page 29",
      "Page 30",
      "Page 31",
      "Page 32",
    ]);
    expect(batch.every((p) => p.origin === "auto")).toBe(true);
    expect(newCollisions(out, before)).toEqual([]);
  });

  it("names further pages on the full 40-page roster without a collision", () => {
    const persisted = new Set(ROSTER_2026_11_05.map((p) => p.id));
    const out = reconcilePages([...ROSTER_2026_11_05], ["next-1", "next-2"]);
    expect(out.slice(40).map((p) => p.name)).toEqual(["Page 40", "Page 41"]);
    expect(newCollisions(out, persisted)).toEqual([]);
    // The fixture really is the 40-page roster with its measured duplicates.
    expect(ROSTER_2026_11_05).toHaveLength(40);
    expect(ROSTER_2026_11_05.filter((p) => p.name === "Page 23")).toHaveLength(3);
    expect(ROSTER_2026_11_05.filter((p) => p.name === "Terminal").map((p) => p.id)).toEqual([
      "default",
      "3e86a8ae",
    ]);
    expect(LATER_PAGE_IDS).toHaveLength(12);
  });
});

describe("resolveProjectPage names a created page uniquely", () => {
  const mint = () => "minted";

  it('a project page "tmux" beside the operator page "tmux" is "tmux 2"', () => {
    const pages: TerminalPageConfig[] = [
      { id: "default", name: "Terminal", createdAt: 0 },
      { id: "op", name: "tmux", createdAt: 1 },
    ];
    const out = resolveProjectPage(pages, null, "tmux", undefined, mint);
    const page = out.pages.find((p) => p.id === "minted")!;
    expect(page.name).toBe("tmux 2");
    expect(page.origin).toBe("operator");
  });

  it('a project called "Terminal" does not take the default page\'s name', () => {
    const pages: TerminalPageConfig[] = [{ id: "default", name: "Terminal", createdAt: 0 }];
    const out = resolveProjectPage(pages, null, "Terminal", undefined, mint);
    expect(out.pages.find((p) => p.id === "minted")!.name).toBe("Page 1");
  });
});
