/**
 * Regression pin for the CommandBar's selection-vs-query invariant.
 *
 * The defect (found on-page through the UI Bridge relay): typing `sp`,
 * pressing ArrowDown twice, then typing `lyt` left `selectedIdx` at 2.
 * The dropdown re-rendered as `[/layout, /analyze, /select-by-state]`,
 * `aria-activedescendant` still pointed at `select-by-state`, and Enter
 * RAN it — a command the operator never selected, with no visible
 * indication that the highlighted row and the executed row had come
 * apart.
 *
 * The old guard only clamped `selectedIdx >= matches.length`, which is a
 * strictly weaker property: the first `describe` below shows a stale
 * index staying comfortably in range while naming a different command.
 * The second `describe` pins the component-side fix — the reset is
 * derived from `query` itself, so it covers every path that mutates the
 * query rather than the one that happened to be noticed.
 *
 * Source-text assertions rather than a render: vitest runs with
 * `environment: "node"` here and the repo ships no DOM test library
 * (same precedent as `SessionManagerToggle.test.ts`).
 */

import { readFileSync } from "fs";
import { join } from "path";

import { afterEach, describe, expect, it } from "vitest";

import { __resetForTest, register } from "./commands/registry";
import { resolve } from "./commands/resolve";
import type { CommandAction } from "./commands/types";

const SOURCE = readFileSync(join(__dirname, "CommandBar.tsx"), "utf8");

afterEach(() => {
  __resetForTest();
});

const action = (id: string, slash: string, label: string): CommandAction => ({
  id,
  slash,
  label,
  description: `${label} — for the selection spec`,
  handler: async () => ({ ok: true }),
});

describe("clamping alone cannot protect the selection", () => {
  it("leaves a stale in-range index pointing at a DIFFERENT command", () => {
    // The three actions from the on-page reproduction.
    register(action("layout", "/layout", "Change layout preset"));
    register(action("analyze", "/analyze", "Analyze terminal output"));
    register(action("select-by-state", "/select-by-state", "Select zones by state"));
    register(action("spawn", "/spawn", "Spawn plain terminal"));
    register(action("spawn-ai", "/spawn-ai", "Spawn AI session"));
    register(action("spawn-with", "/spawn-with", "Spawn terminal with command"));

    const before = resolve("sp", []);
    const after = resolve("lyt", []);

    // Operator had arrowed down to index 2 against the `sp` list.
    const STALE_IDX = 2;
    expect(before.length).toBeGreaterThan(STALE_IDX);

    // The clamp's own condition is FALSE here — the index is in range —
    // so the clamp would have left it alone...
    expect(STALE_IDX < after.length).toBe(true);

    // ...while naming a different command than the one the operator
    // selected, and a different one than the list now leads with.
    expect(after[STALE_IDX].action.id).not.toBe(before[STALE_IDX].action.id);
    expect(after[0].action.id).toBe("layout");
    expect(after[STALE_IDX].action.id).not.toBe("layout");
  });
});

describe("CommandBar resets the selection on every query change", () => {
  it("derives the reset from `query` rather than patching one handler", () => {
    // React's "adjust state during render" pattern, already the idiom in
    // CommandPalette.tsx. Keying off the VALUE is what makes this cover
    // typing, history recall, Tab completion, Escape, a suggestion click
    // and the post-execute clear at once.
    expect(SOURCE).toMatch(
      /if \(query !== prevQuery\) \{\s*setPrevQuery\(query\);\s*setSelectedIdx\(0\);/,
    );
  });

  it("never restores a selection index computed for the previous query", () => {
    // Clicking a suggestion that needs args rewrites the query to
    // `/slash ` and used to follow with `setSelectedIdx(idx)` — but `idx`
    // indexed the OLD match list, and the new query resolves to a single
    // exact row. The handler no longer receives an index at all.
    //
    // (`onMouseEnter` still sets one, legitimately: hovering selects a
    // row inside the CURRENT list, which is not a query change.)
    expect(SOURCE).toMatch(
      /const handleSuggestionClick = useCallback\(\s*\(\s*action: CommandAction,\s*presetArgs\?/,
    );
    expect(SOURCE).toContain("handleSuggestionClick(m.action, m.presetArgs, m.tier)");
  });

  it("keeps the clamp only for list changes that are NOT query changes", () => {
    // Still needed for a registry unregister or the async Tier-3 match
    // being cleared — but it is no longer what carries a query change.
    expect(SOURCE).toContain("if (selectedIdx >= matches.length)");
  });
});
