/**
 * The Ctrl+Shift+K palette's keyboard selection must be readable from
 * OUTSIDE the app.
 *
 * Its rows were `role="button"` with the highlight living only in a
 * Tailwind class — no listbox, no option, no `aria-selected`, no
 * `data-page-element` — so no external driver (the UI Bridge relay, a
 * screen reader) could tell which row Enter would run. CommandBar's
 * suggestion dropdown already had exactly this contract; this mirrors it.
 *
 * `environment: "node"` vitest with no DOM test library in the repo, so
 * these are source assertions (precedent: `SessionManagerToggle.test.ts`).
 */

import { readFileSync } from "fs";
import { join } from "path";

import { describe, expect, it } from "vitest";

const SOURCE = readFileSync(join(__dirname, "CommandPalette.tsx"), "utf8");

describe("CommandPalette — listbox/option contract", () => {
  it("wraps the rows in a listbox", () => {
    expect(SOURCE).toMatch(/id=\{LISTBOX_ID\}\s+role="listbox"/);
  });

  it("renders each row as an option carrying its selected state", () => {
    expect(SOURCE).toContain('role="option"');
    expect(SOURCE).toContain("aria-selected={i === selectedIndex}");
  });

  it("gives each row a stable id AND data-page-element", () => {
    expect(SOURCE).toContain("id={paletteOptionId(action.id)}");
    expect(SOURCE).toContain("data-page-element={paletteOptionId(action.id)}");
    expect(SOURCE).toMatch(/return `command-palette-option-\$\{actionId\}`;/);
  });

  it("points the input at the selected option", () => {
    expect(SOURCE).toContain('role="combobox"');
    expect(SOURCE).toContain("aria-activedescendant={");
    expect(SOURCE).toContain("aria-controls={filtered.length > 0 ? LISTBOX_ID : undefined}");
  });

  it("no longer renders rows as buttons", () => {
    // The backdrop keeps its own `role="button"` (click-outside-to-close);
    // what must be gone is a row-level one.
    const rowButton = /role="button"\s+tabIndex=\{0\}\s+onClick=\{\(\) => executeAction/;
    expect(SOURCE).not.toMatch(rowButton);
  });

  it("keeps the scroll-into-view ref on the listbox itself", () => {
    // `listRef.current.children[selectedIndex]` indexes the option list,
    // so the "Clear recent commands" button must be a SIBLING of the
    // listbox rather than a child of it.
    expect(SOURCE).toMatch(/<div ref=\{listRef\} id=\{LISTBOX_ID\}/);
  });
});
