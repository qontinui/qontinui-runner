/**
 * `planRegroup` — which windows `/regroup <per-tab>` closes, where every
 * survivor lands, and which tabs are created or removed.
 *
 * vitest runs `environment: "node"` here — pure-function tests only.
 */

import { describe, it, expect } from "vitest";
import { planRegroup, screenHasContent, type RegroupItem, type RegroupKind } from "./regroupTabs";

const item = (id: string, kind: RegroupKind, movable = true): RegroupItem => ({
  id,
  title: id,
  kind,
  movable,
});
const ais = (prefix: string, n: number) =>
  Array.from({ length: n }, (_, i) => item(`${prefix}${i + 1}`, "ai"));

describe("planRegroup", () => {
  it("the requested example: 12 AI | empty | content+empty+AI, per tab 6", () => {
    const plan = planRegroup(
      [
        { pageId: "tab1", items: ais("a", 12) },
        { pageId: "tab2", items: [] },
        {
          pageId: "tab3",
          items: [item("busy", "content"), item("blank", "empty"), item("ai13", "ai")],
        },
      ],
      6,
    );

    expect(plan.close).toEqual([{ pageId: "tab3", id: "blank", title: "blank" }]);
    expect(plan.targets).toEqual([
      { pageId: "tab1", tabIds: ["a1", "a2", "a3", "a4", "a5", "a6"] },
      { pageId: "tab2", tabIds: ["a7", "a8", "a9", "a10", "a11", "a12"] },
      { pageId: "tab3", tabIds: ["busy", "ai13"] },
    ]);
    expect(plan.removePageIds).toEqual([]);
  });

  it("removes tabs left empty once everything fits in fewer", () => {
    const plan = planRegroup(
      [
        { pageId: "p1", items: [item("a", "ai")] },
        { pageId: "p2", items: [item("b", "ai"), item("x", "empty")] },
        { pageId: "p3", items: [] },
      ],
      4,
    );
    expect(plan.targets).toEqual([{ pageId: "p1", tabIds: ["a", "b"] }]);
    expect(plan.removePageIds).toEqual(["p2", "p3"]);
  });

  it("adds tabs when the existing ones run out", () => {
    const plan = planRegroup([{ pageId: "p1", items: ais("a", 5) }], 2);
    expect(plan.targets).toEqual([
      { pageId: "p1", tabIds: ["a1", "a2"] },
      { pageId: null, tabIds: ["a3", "a4"] },
      { pageId: null, tabIds: ["a5"] },
    ]);
  });

  it("keeps an unmovable window on its own tab, using up that tab's slots", () => {
    const plan = planRegroup(
      [
        { pageId: "p1", items: ais("a", 3) },
        { pageId: "p2", items: [item("worker", "ai", false), item("b", "ai")] },
      ],
      2,
    );
    expect(plan.targets).toEqual([
      { pageId: "p1", tabIds: ["a1", "a2"] },
      // The worker stays on p2 and fills one of its two slots.
      { pageId: "p2", tabIds: ["a3", "worker"] },
      { pageId: null, tabIds: ["b"] },
    ]);
    expect(plan.removePageIds).toEqual([]);
  });

  it("never closes an unmovable window, even with an empty screen", () => {
    const plan = planRegroup([{ pageId: "p1", items: [item("plan", "empty", false)] }], 3);
    expect(plan.close).toEqual([]);
    expect(plan.targets).toEqual([{ pageId: "p1", tabIds: ["plan"] }]);
  });

  it("nothing kept: closes the empties and leaves the first tab, empty", () => {
    const plan = planRegroup(
      [
        { pageId: "p1", items: [item("x", "empty")] },
        { pageId: "p2", items: [item("y", "empty")] },
      ],
      3,
    );
    expect(plan.close.map((c) => c.id)).toEqual(["x", "y"]);
    expect(plan.targets).toEqual([{ pageId: "p1", tabIds: [] }]);
    expect(plan.removePageIds).toEqual(["p2"]);
  });
});

describe("screenHasContent", () => {
  it("a bare prompt is not content", () => {
    expect(screenHasContent(["PS D:\\qontinui-root> ", "", ""])).toBe(false);
    expect(screenHasContent(["", "  ", ""])).toBe(false);
  });

  it("a command still running silently on the prompt line is content", () => {
    // One line, but closing it would kill the job.
    expect(screenHasContent(["PS C:\\work> python long_job.py", ""])).toBe(true);
  });

  it("bare prompts of other shells are not content", () => {
    expect(screenHasContent(["user@box:~$ "])).toBe(false);
    expect(screenHasContent(["root@box:/# "])).toBe(false);
    expect(screenHasContent(["~/src ❯ "])).toBe(false);
  });

  it("an unrecognised single line counts as content (errs toward keeping)", () => {
    expect(screenHasContent(["Starting server..."])).toBe(true);
  });

  it("a command and its output, or the next prompt, is content", () => {
    expect(screenHasContent(["PS D:\\> ls", "    Directory: D:\\", ""])).toBe(true);
    expect(screenHasContent(["PS D:\\> git status", "PS D:\\> "])).toBe(true);
  });
});
