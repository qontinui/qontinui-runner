/**
 * Every `Ctrl+Shift+<key>` chord on the terminal page must be matched
 * case-insensitively.
 *
 * With CapsLock on, Shift INVERTS the case, so the browser reports
 * `e.key === "b"` for Ctrl+Shift+B. Only Ctrl+Shift+K had been routed
 * through `isCtrlShiftChord` (it was normalised because it collided with
 * the KnowledgeBrowser, not because anyone audited the rest); the ~20
 * chords around it still compared against a literal uppercase letter and
 * were therefore DEAD under CapsLock — new terminal, close, maximize,
 * swap, restart, layout cycle, the session sidebar, all of it.
 *
 * `environment: "node"` vitest with no DOM test library in the repo, so
 * this pins the source-level property (same precedent as
 * `SessionManagerToggle.test.ts`); the predicate's own behaviour is
 * covered by `lib/globalChords.test.ts`.
 */

import { readFileSync } from "fs";
import { join } from "path";

import { describe, expect, it } from "vitest";

import { isCtrlShiftChord } from "@/lib/globalChords";

const SOURCE = readFileSync(join(__dirname, "useKeyboardShortcuts.ts"), "utf8");

describe("useKeyboardShortcuts — chord matching", () => {
  it('has no literal `e.key === "X"` Ctrl+Shift comparison left', () => {
    const literals = SOURCE.match(/e\.ctrlKey && e\.shiftKey && e\.key === /g) ?? [];
    expect(literals).toEqual([]);
  });

  it("routes every Ctrl+Shift chord through the shared predicate", () => {
    const chords = SOURCE.match(/isCtrlShiftChord\(e, [^)]+\)/g) ?? [];
    // The table-driven palette chord plus every inline terminal chord.
    expect(chords.length).toBeGreaterThanOrEqual(20);
    expect(chords).toContain("isCtrlShiftChord(e, GLOBAL_CHORDS.commandPalette)");
    for (const letter of [
      "b",
      "t",
      "w",
      "n",
      "f",
      "m",
      "a",
      "s",
      "x",
      "o",
      "d",
      "r",
      "l",
      "i",
      "p",
      "g",
      "h",
      "j",
    ]) {
      expect(chords).toContain(`isCtrlShiftChord(e, "${letter}")`);
    }
    for (const named of ["Enter", "/", "?", "ArrowLeft", "ArrowRight"]) {
      expect(chords).toContain(`isCtrlShiftChord(e, "${named}")`);
    }
  });

  it("passes every chord letter in lowercase, so a CapsLock `e.key` matches", () => {
    // The predicate lowercases both sides, so an uppercase literal would
    // still work — but the lowercase spelling is what makes the intent
    // ("this is the key the browser reports", not "this is Shift+b")
    // survive the next edit.
    const letters = [...SOURCE.matchAll(/isCtrlShiftChord\(e, "([a-zA-Z])"\)/g)].map((m) => m[1]);
    expect(letters.length).toBeGreaterThan(0);
    for (const letter of letters) {
      expect(letter).toBe(letter.toLowerCase());
    }
  });

  it("still matches when CapsLock lowercased the key (the actual failure)", () => {
    expect(isCtrlShiftChord({ key: "b", ctrlKey: true, shiftKey: true }, "b")).toBe(true);
    expect(isCtrlShiftChord({ key: "B", ctrlKey: true, shiftKey: true }, "b")).toBe(true);
  });
});
