/**
 * `sessionSelection` — the Session Manager's multi-select rules (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 5). The collapsed-group range case is in the gate file,
 * `sessionBulkActions.test.ts`.
 */

import { describe, expect, it } from "vitest";

import {
  EMPTY_SELECTION,
  applyListKey,
  clearSelection,
  clickRow,
  listKeyAction,
  pruneSelection,
  renderedRowIds,
  selectAll,
  toggleRow,
  visibleRange,
  type SelectionState,
} from "./sessionSelection";

const VISIBLE = ["a", "b", "c", "d", "e"];
const plain = { toggle: false, range: false };
const ctrl = { toggle: true, range: false };
const shift = { toggle: false, range: true };
const ctrlShift = { toggle: true, range: true };

const ids = (s: SelectionState) => [...s.ids].sort();

describe("visibleRange", () => {
  it("spans the anchor to the row inclusive, either direction", () => {
    expect(visibleRange(VISIBLE, "b", "d")).toEqual(["b", "c", "d"]);
    expect(visibleRange(VISIBLE, "d", "b")).toEqual(["b", "c", "d"]);
    expect(visibleRange(VISIBLE, "c", "c")).toEqual(["c"]);
  });

  it("an anchor that is not shown ranges over the row alone; a row not shown, nothing", () => {
    expect(visibleRange(VISIBLE, null, "c")).toEqual(["c"]);
    expect(visibleRange(VISIBLE, "hidden", "c")).toEqual(["c"]);
    expect(visibleRange(VISIBLE, "a", "hidden")).toEqual([]);
  });
});

describe("clickRow", () => {
  it("a plain click opens the row, anchors it and clears the selection", () => {
    const start = { ids: new Set(["a", "b"]), anchor: "a", cursor: "a" };
    const step = clickRow(start, VISIBLE, "c", plain);
    expect(step.open).toBe("c");
    expect(step.state).toEqual({ ids: new Set(), anchor: "c", cursor: "c" });
  });

  it("Ctrl-click toggles one row and never opens it", () => {
    const one = clickRow(EMPTY_SELECTION, VISIBLE, "b", ctrl);
    expect(one.open).toBeNull();
    expect(ids(one.state)).toEqual(["b"]);
    const two = clickRow(one.state, VISIBLE, "d", ctrl);
    expect(ids(two.state)).toEqual(["b", "d"]);
    expect(ids(clickRow(two.state, VISIBLE, "b", ctrl).state)).toEqual(["d"]);
  });

  it("Shift-click replaces the selection with the range from the anchor", () => {
    const anchored = clickRow({ ...EMPTY_SELECTION, ids: new Set(["e"]) }, VISIBLE, "b", ctrl);
    const ranged = clickRow(anchored.state, VISIBLE, "d", shift);
    expect(ids(ranged.state)).toEqual(["b", "c", "d"]);
    expect(ranged.state.anchor).toBe("b");
    expect(ranged.state.cursor).toBe("d");
  });

  it("Ctrl+Shift-click adds the range to the selection", () => {
    const start: SelectionState = { ids: new Set(["e"]), anchor: "a", cursor: "a" };
    expect(ids(clickRow(start, VISIBLE, "b", ctrlShift).state)).toEqual(["a", "b", "e"]);
  });

  it("a Shift-click with no anchor selects just that row and anchors it", () => {
    const step = clickRow(EMPTY_SELECTION, VISIBLE, "c", shift);
    expect(ids(step.state)).toEqual(["c"]);
    expect(step.state.anchor).toBe("c");
  });
});

describe("selection helpers", () => {
  it("toggleRow, selectAll, clearSelection", () => {
    const t = toggleRow(EMPTY_SELECTION, "a");
    expect(ids(t)).toEqual(["a"]);
    expect(ids(toggleRow(t, "a"))).toEqual([]);
    expect(ids(selectAll(t, VISIBLE))).toEqual(VISIBLE);
    expect(ids(clearSelection(selectAll(t, VISIBLE)))).toEqual([]);
  });

  it("pruneSelection drops rows that left the list, and keeps identity when nothing did", () => {
    const s: SelectionState = { ids: new Set(["a", "gone"]), anchor: null, cursor: null };
    expect(ids(pruneSelection(s, new Set(VISIBLE)))).toEqual(["a"]);
    const kept: SelectionState = { ids: new Set(["a"]), anchor: null, cursor: null };
    expect(pruneSelection(kept, new Set(VISIBLE))).toBe(kept);
  });

  it("renderedRowIds flattens the sections in order", () => {
    expect(
      renderedRowIds([
        { sessions: [{ sessionId: "p" }] },
        { sessions: [] },
        { sessions: [{ sessionId: "x" }, { sessionId: "y" }] },
      ]),
    ).toEqual(["p", "x", "y"]);
  });
});

describe("the list's keys", () => {
  const key = (
    k: string,
    mods: { ctrlKey?: boolean; metaKey?: boolean; shiftKey?: boolean; altKey?: boolean } = {},
  ) => listKeyAction({ key: k, ctrlKey: false, shiftKey: false, ...mods });

  it("maps the bare keys", () => {
    expect(key("ArrowDown")).toBe("down");
    expect(key("ArrowUp")).toBe("up");
    expect(key("Enter")).toBe("open");
    expect(key(" ")).toBe("toggle");
    expect(key("Escape")).toBe("clear");
    expect(key("x")).toBeNull();
  });

  it("Ctrl/⌘+A selects all; any other modified key is left alone", () => {
    expect(key("a", { ctrlKey: true })).toBe("select-all");
    expect(key("A", { metaKey: true })).toBe("select-all");
    expect(key("a", { ctrlKey: true, shiftKey: true })).toBeNull();
    expect(key("ArrowDown", { ctrlKey: true })).toBeNull();
    expect(key("ArrowDown", { altKey: true })).toBeNull();
  });

  it("↑/↓ move the cursor and stop at the ends", () => {
    let s: SelectionState = EMPTY_SELECTION;
    s = applyListKey(s, VISIBLE, "down").state;
    expect(s.cursor).toBe("a");
    s = applyListKey(s, VISIBLE, "up").state;
    expect(s.cursor).toBe("a");
    expect(applyListKey(EMPTY_SELECTION, VISIBLE, "up").state.cursor).toBe("e");
    s = { ...s, cursor: "e" };
    expect(applyListKey(s, VISIBLE, "down").state.cursor).toBe("e");
    expect(applyListKey(EMPTY_SELECTION, [], "down").state).toBe(EMPTY_SELECTION);
  });

  it("Enter opens the cursor row; Space toggles it; nothing happens without a cursor", () => {
    const at: SelectionState = { ...EMPTY_SELECTION, cursor: "c" };
    expect(applyListKey(at, VISIBLE, "open").open).toBe("c");
    expect(ids(applyListKey(at, VISIBLE, "toggle").state)).toEqual(["c"]);
    expect(applyListKey(EMPTY_SELECTION, VISIBLE, "open").open).toBeNull();
    expect(applyListKey(EMPTY_SELECTION, VISIBLE, "toggle").state).toBe(EMPTY_SELECTION);
  });

  it("a cursor on a row no longer shown restarts from the end", () => {
    const stale: SelectionState = { ...EMPTY_SELECTION, cursor: "hidden" };
    expect(applyListKey(stale, VISIBLE, "down").state.cursor).toBe("a");
    expect(applyListKey(stale, VISIBLE, "open").open).toBeNull();
  });
});
