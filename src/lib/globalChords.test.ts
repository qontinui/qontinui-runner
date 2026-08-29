/**
 * Tests for the shared global-chord table + predicates.
 *
 * Three live UI-Bridge findings are pinned here:
 *   1. `Ctrl+Shift+K` opened the CommandPalette AND the KnowledgeBrowser
 *      at once — both surfaces claimed the same letter.
 *   2. The two handlers matched different cases (`"K"` vs `"K" || "k"`),
 *      so with CapsLock on the chord half-fired.
 *   3. `unified-search/CommandPalette` claimed Cmd/Ctrl+K with no
 *      `shiftKey` test, so `Ctrl+Shift+k` opened it ON TOP of the
 *      terminal palette. Two chords may share a letter — but only if
 *      their modifiers differ, and only if both are matched exactly.
 */

import { describe, it, expect } from "vitest";
import {
  GLOBAL_CHORDS,
  isCtrlShiftChord,
  matchesChord,
  type ChordKeyLike,
  type GlobalChord,
} from "./globalChords";

const key = (over: Partial<ChordKeyLike> = {}): ChordKeyLike => ({
  key: "k",
  ctrlKey: true,
  shiftKey: true,
  ...over,
});

/** The full spelling of a chord, which is what must be unique. */
const spelling = (c: GlobalChord) => `${c.shift ? "shift+" : ""}${c.key.toLowerCase()}`;

describe("GLOBAL_CHORDS", () => {
  it("assigns a distinct SPELLING to every cross-tree surface", () => {
    // Not a distinct letter — `commandPalette` and `unifiedSearch` both
    // use "k". What must not repeat is modifiers+key.
    const spellings = Object.values(GLOBAL_CHORDS).map(spelling);
    expect(new Set(spellings).size).toBe(spellings.length);
  });

  it("keeps the knowledge browser off the command palette's chord", () => {
    expect(spelling(GLOBAL_CHORDS.knowledgeBrowser)).not.toBe(
      spelling(GLOBAL_CHORDS.commandPalette),
    );
  });

  it("does not let one keypress satisfy two surfaces", () => {
    for (const chord of Object.values(GLOBAL_CHORDS)) {
      for (const pressed of [chord.key, chord.key.toUpperCase()]) {
        const e = key({ key: pressed, shiftKey: chord.shift, metaKey: false });
        const hits = Object.entries(GLOBAL_CHORDS)
          .filter(([, c]) => matchesChord(e, c))
          .map(([name]) => name);
        expect(hits).toEqual([Object.entries(GLOBAL_CHORDS).find(([, c]) => c === chord)?.[0]]);
      }
    }
  });

  it("keeps Ctrl+Shift+K off unified search in BOTH cases", () => {
    // The reported bug: Shift+CapsLock reports "k", and the shiftless
    // handler had no `shiftKey` test, so the lowercase spelling fired.
    for (const pressed of ["k", "K"]) {
      const e = key({ key: pressed, shiftKey: true });
      expect(matchesChord(e, GLOBAL_CHORDS.unifiedSearch)).toBe(false);
      expect(matchesChord(e, GLOBAL_CHORDS.commandPalette)).toBe(true);
    }
  });

  it("keeps a shiftless Ctrl+K off the terminal palette in BOTH cases", () => {
    for (const pressed of ["k", "K"]) {
      const e = key({ key: pressed, shiftKey: false });
      expect(matchesChord(e, GLOBAL_CHORDS.unifiedSearch)).toBe(true);
      expect(matchesChord(e, GLOBAL_CHORDS.commandPalette)).toBe(false);
    }
  });
});

describe("matchesChord", () => {
  it("matches Cmd only for a chord that declares meta", () => {
    const cmdK = key({ key: "k", shiftKey: false, ctrlKey: false, metaKey: true });
    expect(matchesChord(cmdK, GLOBAL_CHORDS.unifiedSearch)).toBe(true);
    // Terminal chords are Ctrl-only: the Windows key must not spell them.
    const winShiftK = key({ key: "k", shiftKey: true, ctrlKey: false, metaKey: true });
    expect(matchesChord(winShiftK, GLOBAL_CHORDS.commandPalette)).toBe(false);
  });

  it("requires the control modifier", () => {
    expect(
      matchesChord(key({ ctrlKey: false, shiftKey: false, metaKey: false }), {
        key: "k",
        shift: false,
        meta: false,
      }),
    ).toBe(false);
  });
});

describe("isCtrlShiftChord", () => {
  it("matches the lowercase key Shift normally produces", () => {
    expect(isCtrlShiftChord(key({ key: "k" }), GLOBAL_CHORDS.commandPalette.key)).toBe(true);
  });

  it("matches the uppercase key CapsLock flips it to", () => {
    // Shift+CapsLock yields "k"; Shift alone yields "K". Both are the
    // same chord to the operator, so both must match.
    expect(isCtrlShiftChord(key({ key: "K" }), GLOBAL_CHORDS.commandPalette.key)).toBe(true);
    expect(isCtrlShiftChord(key({ key: "E" }), GLOBAL_CHORDS.knowledgeBrowser.key)).toBe(true);
    expect(isCtrlShiftChord(key({ key: "e" }), GLOBAL_CHORDS.knowledgeBrowser.key)).toBe(true);
  });

  it("requires both modifiers", () => {
    expect(isCtrlShiftChord(key({ ctrlKey: false }), "k")).toBe(false);
    expect(isCtrlShiftChord(key({ shiftKey: false }), "k")).toBe(false);
  });

  it("does not match a different letter", () => {
    expect(isCtrlShiftChord(key({ key: "j" }), "k")).toBe(false);
  });
});
