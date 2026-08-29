/**
 * Tests for the shared global-chord table + predicate.
 *
 * Two live UI-Bridge findings are pinned here:
 *   1. `Ctrl+Shift+K` opened the CommandPalette AND the KnowledgeBrowser
 *      at once — both surfaces claimed the same letter.
 *   2. The two handlers matched different cases (`"K"` vs `"K" || "k"`),
 *      so with CapsLock on the chord half-fired.
 */

import { describe, it, expect } from "vitest";
import { GLOBAL_CHORDS, isCtrlShiftChord, type ChordKeyLike } from "./globalChords";

const key = (over: Partial<ChordKeyLike> = {}): ChordKeyLike => ({
  key: "k",
  ctrlKey: true,
  shiftKey: true,
  ...over,
});

describe("GLOBAL_CHORDS", () => {
  it("assigns a distinct letter to every cross-tree surface", () => {
    const letters = Object.values(GLOBAL_CHORDS);
    expect(new Set(letters).size).toBe(letters.length);
  });

  it("keeps the knowledge browser off the command palette's chord", () => {
    expect(GLOBAL_CHORDS.knowledgeBrowser).not.toBe(GLOBAL_CHORDS.commandPalette);
  });

  it("does not let one keypress satisfy two surfaces", () => {
    for (const letter of Object.values(GLOBAL_CHORDS)) {
      const e = key({ key: letter });
      const hits = Object.values(GLOBAL_CHORDS).filter((l) => isCtrlShiftChord(e, l));
      expect(hits).toEqual([letter]);
    }
  });
});

describe("isCtrlShiftChord", () => {
  it("matches the lowercase key Shift normally produces", () => {
    expect(isCtrlShiftChord(key({ key: "k" }), GLOBAL_CHORDS.commandPalette)).toBe(true);
  });

  it("matches the uppercase key CapsLock flips it to", () => {
    // Shift+CapsLock yields "k"; Shift alone yields "K". Both are the
    // same chord to the operator, so both must match.
    expect(isCtrlShiftChord(key({ key: "K" }), GLOBAL_CHORDS.commandPalette)).toBe(true);
    expect(isCtrlShiftChord(key({ key: "E" }), GLOBAL_CHORDS.knowledgeBrowser)).toBe(true);
    expect(isCtrlShiftChord(key({ key: "e" }), GLOBAL_CHORDS.knowledgeBrowser)).toBe(true);
  });

  it("requires both modifiers", () => {
    expect(isCtrlShiftChord(key({ ctrlKey: false }), "k")).toBe(false);
    expect(isCtrlShiftChord(key({ shiftKey: false }), "k")).toBe(false);
  });

  it("does not match a different letter", () => {
    expect(isCtrlShiftChord(key({ key: "j" }), "k")).toBe(false);
  });
});
