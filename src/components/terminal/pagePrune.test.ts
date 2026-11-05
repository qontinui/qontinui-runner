/**
 * The pure half of the empty-auto-page prune and the one-time roster repair
 * (plan 2026-11-05 Phase 6). The mounted-hook half is
 * `useTerminalPages.prune.test.tsx`.
 */

import { describe, expect, it } from "vitest";
import { OPERATOR_PAGE_NAMES, ROSTER_2026_11_05 } from "./fixtures/terminalPageRoster";
import {
  PRUNE_MIN_AGE_MS,
  classifyLegacyOrigin,
  duplicateNameRepairs,
  prunablePageIds,
  pruneNoticeText,
  removeEach,
  restoreRemovedPages,
  withClassifiedOrigins,
  type RemovedPage,
} from "./pagePrune";
import type { TerminalPageConfig } from "./useTerminalPages";

const NOW = Date.UTC(2026, 10, 5, 12);
const OLD = NOW - PRUNE_MIN_AGE_MS;
const page = (id: string, name: string, extra: Partial<TerminalPageConfig> = {}) =>
  ({ id, name, createdAt: OLD, ...extra }) as TerminalPageConfig;
const guards = { activePageId: "default", boundPages: {}, now: NOW };

describe("classifyLegacyOrigin (D2 (a))", () => {
  it("classifies the 2026-11-05 roster: the six operator-named pages are operator", () => {
    const classified = withClassifiedOrigins([...ROSTER_2026_11_05]);
    const operator = classified.filter((p) => p.origin === "operator").map((p) => p.name);
    expect(operator).toEqual(["Terminal", ...OPERATOR_PAGE_NAMES]);
    expect(classified.find((p) => p.id === "3e86a8ae")?.origin).toBe("auto");
    expect(classified.find((p) => p.id === "oct-page-23b")?.origin).toBe("auto");
  });

  it("reads Page <n>, Terminal and Restored <8 hex> as auto, anything else as operator", () => {
    expect(classifyLegacyOrigin(page("a", "Page 7"))).toBe("auto");
    expect(classifyLegacyOrigin(page("b", "Terminal"))).toBe("auto");
    expect(classifyLegacyOrigin(page("c", "Restored 1a2b3c4d"))).toBe("auto");
    expect(classifyLegacyOrigin(page("d", "Restored 1A2B3C4D"))).toBe("operator");
    expect(classifyLegacyOrigin(page("e", "Page 7b"))).toBe("operator");
    expect(classifyLegacyOrigin(page("f", "Page 7", { defaultWorkingDir: "/repo" }))).toBe(
      "operator",
    );
    expect(classifyLegacyOrigin(page("default", "Terminal"))).toBe("operator");
  });

  it("keeps an origin a page already carries", () => {
    expect(classifyLegacyOrigin(page("a", "resumes", { origin: "auto" }))).toBe("auto");
    const pages = [page("a", "Page 1", { origin: "operator" })];
    expect(withClassifiedOrigins(pages)).toBe(pages);
  });
});

describe("prunablePageIds (D3)", () => {
  const auto = (id: string, extra: Partial<TerminalPageConfig> = {}) =>
    page(id, `Page ${id}`, { origin: "auto", ...extra });

  it("takes empty, old, auto pages only", () => {
    const pages = [
      page("default", "Terminal"),
      auto("1"),
      page("op", "resumes", { origin: "operator" }),
      auto("2"),
      auto("3"),
    ];
    expect(prunablePageIds(pages, new Set(["3"]), guards)).toEqual(["1", "2"]);
  });

  it("never takes an operator page, however empty", () => {
    // "busy" is occupied, so the keep-one rule does not mask the origin filter.
    const pages = [page("op", "resumes", { origin: "operator" }), auto("1"), auto("busy")];
    expect(prunablePageIds(pages, new Set(["busy"]), guards)).toEqual(["1"]);
  });

  it("never takes the active page or a page bound to a pop-out", () => {
    const pages = [auto("1"), auto("2"), auto("3"), auto("4")];
    expect(
      prunablePageIds(pages, new Set(), {
        activePageId: "2",
        boundPages: { "3": "terminal-popout-1" },
        now: NOW,
      }),
    ).toEqual(["1", "4"]);
  });

  it("waits until a page is PRUNE_MIN_AGE_MS old", () => {
    expect(PRUNE_MIN_AGE_MS).toBe(300_000);
    const pages = [auto("old"), auto("young", { createdAt: NOW - 60_000 }), auto("keep")];
    expect(prunablePageIds(pages, new Set(["keep"]), guards)).toEqual(["old"]);
  });

  it("keeps one non-default page when every one is a candidate", () => {
    const pages = [page("default", "Terminal"), auto("1"), auto("2")];
    expect(prunablePageIds(pages, new Set(), guards)).toEqual(["2"]);
  });

  it("counts only docked pages for the keep-one rule", () => {
    const pages = [auto("1"), auto("2"), auto("bound")];
    const bound = { ...guards, boundPages: { bound: "terminal-popout-1" } };
    expect(prunablePageIds(pages, new Set(), bound)).toEqual(["2"]);
  });
});

describe("duplicateNameRepairs (D2 (b))", () => {
  it("renames a non-default Terminal, and every later holder of a shared name", () => {
    const pages = [
      page("default", "Terminal", { createdAt: 0 }),
      page("t2", "Terminal", { createdAt: 5 }),
      page("p5-new", "Page 5", { createdAt: 9 }),
      page("p5-old", "Page 5", { createdAt: 3 }),
      page("tmux", "tmux", { createdAt: 1 }),
      page("tmux-b", "tmux", { createdAt: 2 }),
    ];
    expect(duplicateNameRepairs(pages, new Set())).toEqual([
      { id: "t2", from: "Terminal", to: "Page 6" },
      { id: "tmux-b", from: "tmux", to: "tmux 2" },
      // A runner name is renamed by the runner-name rule, not suffixed.
      { id: "p5-new", from: "Page 5", to: "Page 7" },
    ]);
  });

  it("lets an occupied page keep its name over an older empty one", () => {
    const pages = [
      page("oct", "Page 20", { createdAt: 1, origin: "auto" }),
      page("nov", "Page 20", { createdAt: 9, origin: "auto" }),
    ];
    expect(duplicateNameRepairs(pages, new Set(["nov"]))).toEqual([
      { id: "oct", from: "Page 20", to: "Page 21" },
    ]);
  });

  it("lets an operator page keep its name over an older auto one", () => {
    const pages = [
      page("a", "tmux", { createdAt: 1, origin: "auto" }),
      page("b", "tmux", { createdAt: 9, origin: "operator" }),
    ];
    expect(duplicateNameRepairs(pages, new Set())).toEqual([
      { id: "a", from: "tmux", to: "tmux 2" },
    ]);
  });

  it("changes nothing on a clean roster", () => {
    expect(
      duplicateNameRepairs([page("default", "Terminal"), page("a", "Page 1")], new Set()),
    ).toEqual([]);
  });
});

describe("restoreRemovedPages (Undo)", () => {
  it("puts pages back at their prior indices, as operator pages", () => {
    const original = [page("a", "A"), page("b", "B"), page("c", "C"), page("d", "D")];
    // Removed b (index 1) then d (index 2 of the shortened roster).
    const removed: RemovedPage[] = [
      { page: { ...original[1], origin: "auto" }, index: 1, entries: [] },
      { page: { ...original[3], origin: "auto" }, index: 2, entries: [] },
    ];
    const restored = restoreRemovedPages([original[0], original[2]], removed);
    expect(restored.map((p) => p.id)).toEqual(["a", "b", "c", "d"]);
    expect(restored[1].origin).toBe("operator");
    expect(restored[3].origin).toBe("operator");
  });

  it("does not bring back a duplicate name or a second Terminal", () => {
    const removed: RemovedPage[] = [
      { page: page("t", "Terminal", { origin: "auto" }), index: 0, entries: [] },
      { page: page("x", "Page 3", { origin: "auto" }), index: 0, entries: [] },
    ];
    const restored = restoreRemovedPages(
      [page("default", "Terminal"), page("y", "Page 3")],
      removed,
    );
    expect(restored.map((p) => p.name)).toEqual(["Page 4", "Page 3 2", "Terminal", "Page 3"]);
  });
});

describe("removeEach", () => {
  const read = (...ids: string[]) => ({ ids: new Set(ids), complete: true });
  it("reports a pass that met an incomplete re-read", async () => {
    const reads = [read(), { ids: new Set<string>(), complete: false }];
    const pass = await removeEach(
      ["a", "b"],
      async () => reads.shift()!,
      async (id) => id,
    );
    expect(pass).toEqual({ removed: ["a"], allReadsComplete: false, lastIds: new Set() });
  });
  it("reports a whole pass as complete", async () => {
    const pass = await removeEach(
      ["a", "b"],
      async () => read("b"),
      async (id) => id,
    );
    expect(pass).toEqual({ removed: ["a"], allReadsComplete: true, lastIds: new Set(["b"]) });
  });
});

describe("pruneNoticeText", () => {
  it("names the count and every rename", () => {
    expect(pruneNoticeText(12, [])).toBe("Removed 12 empty auto-created pages");
    expect(pruneNoticeText(1, [{ id: "t", from: "Terminal", to: "Page 6" }])).toBe(
      'Removed 1 empty auto-created page · Renamed "Terminal" → "Page 6"',
    );
    expect(pruneNoticeText(0, [{ id: "a", from: "tmux", to: "tmux 2" }])).toBe(
      'Renamed "tmux" → "tmux 2"',
    );
  });
});
