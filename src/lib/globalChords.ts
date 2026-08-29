/**
 * globalChords — the app-wide `Ctrl+Shift+<letter>` bindings that are
 * registered on `window` from more than one place, plus the one
 * predicate every such handler must use to test for them.
 *
 * Why this exists: `useKeyboardShortcuts` (terminal) and
 * `useKnowledgeBrowserHotkey` (productivity) both attach their own
 * `keydown` listener to `window`, and both claimed `Ctrl+Shift+K`. Two
 * listeners on the SAME target both run — `stopPropagation()` does not
 * suppress a sibling listener on that target — so the chord opened the
 * CommandPalette AND the KnowledgeBrowser at once. Owning the letters
 * in one table is what keeps a second surface from silently claiming a
 * chord that is already spoken for.
 *
 * The handlers also disagreed on CASE: one tested `e.key === "K"`, the
 * other `e.key === "K" || e.key === "k"`. With CapsLock on, Shift makes
 * `e.key` lowercase, so the chord half-fired — one surface opened and
 * the other did not. {@link isCtrlShiftChord} normalises the case once
 * so no caller has to remember.
 *
 * Leaf module (no React, no DOM beyond the `KeyboardEvent` shape) so it
 * is unit-testable under vitest's `environment: "node"` — same rationale
 * as `components/terminal/scrollKeys.ts`.
 */

/** The subset of `KeyboardEvent` the predicate depends on. */
export interface ChordKeyLike {
  key: string;
  ctrlKey: boolean;
  shiftKey: boolean;
}

/**
 * Global `Ctrl+Shift+<letter>` chords claimed by surfaces that live in
 * different component trees. Terminal-only chords stay inline in
 * `useKeyboardShortcuts` — this table is for the cross-tree ones, which
 * are the only ones that can collide unnoticed.
 */
export const GLOBAL_CHORDS = {
  /** Terminal command palette (`useKeyboardShortcuts` → `TOGGLE_COMMAND_PALETTE`). */
  commandPalette: "k",
  /** Productivity knowledge browser (`useKnowledgeBrowserHotkey`). */
  knowledgeBrowser: "e",
} as const;

/**
 * True when the event is `Ctrl+Shift+<letter>`, case-insensitively.
 *
 * Alt is deliberately not inspected — none of the chords above are
 * Alt-qualified, and the existing terminal handlers don't test it
 * either; adding the check here alone would make the two disagree.
 */
export function isCtrlShiftChord(e: ChordKeyLike, letter: string): boolean {
  return e.ctrlKey && e.shiftKey && e.key.toLowerCase() === letter.toLowerCase();
}
