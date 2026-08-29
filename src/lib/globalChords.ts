/**
 * globalChords — the app-wide keyboard chords that are registered on
 * `window`/`document` from more than one place, plus the one predicate
 * every such handler must use to test for them.
 *
 * Why this exists: `useKeyboardShortcuts` (terminal) and
 * `useKnowledgeBrowserHotkey` (productivity) both attach their own
 * `keydown` listener to `window`, and both claimed `Ctrl+Shift+K`. Two
 * listeners on the SAME target both run — `stopPropagation()` does not
 * suppress a sibling listener on that target — so the chord opened the
 * CommandPalette AND the KnowledgeBrowser at once. Owning the chords in
 * one table is what keeps a second surface from silently claiming one
 * that is already spoken for.
 *
 * The handlers also disagreed on CASE: one tested `e.key === "K"`, the
 * other `e.key === "K" || e.key === "k"`. With CapsLock on, Shift makes
 * `e.key` lowercase, so the chord half-fired — one surface opened and
 * the other did not. {@link matchesChord} normalises the case once so no
 * caller has to remember.
 *
 * A THIRD listener then turned up outside the table:
 * `unified-search/CommandPalette` tested `(e.metaKey || e.ctrlKey) &&
 * e.key === "k"` on `document`, with no `shiftKey` test at all. Because
 * Shift+CapsLock reports a lowercase `"k"`, `Ctrl+Shift+k` opened the
 * unified-search modal ON TOP of the terminal palette and its `autoFocus`
 * input stole the caret — the exact CapsLock spelling this module was
 * introduced to close, in a file the letter table did not reach. That is
 * why {@link GLOBAL_CHORDS} entries now carry their MODIFIERS rather than
 * a bare letter: `Ctrl+K` and `Ctrl+Shift+K` are different chords that
 * share a letter, so a letter alone cannot say whether two surfaces
 * collide. `globalChords.enforcement.test.ts` scans the source tree and
 * fails when a global chord handler is hand-rolled instead of routed
 * through the predicates here.
 *
 * Leaf module (no React, no DOM beyond the `KeyboardEvent` shape) so it
 * is unit-testable under vitest's `environment: "node"` — same rationale
 * as `components/terminal/scrollKeys.ts`.
 */

/** The subset of `KeyboardEvent` the predicates depend on. */
export interface ChordKeyLike {
  key: string;
  ctrlKey: boolean;
  shiftKey: boolean;
  metaKey?: boolean;
}

/** A chord's spelling: the key the browser reports, plus its modifiers. */
export interface GlobalChord {
  /** `e.key` the chord is spelled with, lowercase. Matched case-insensitively. */
  key: string;
  /** Whether Shift is PART of the chord. `false` means Shift must be ABSENT. */
  shift: boolean;
  /** Whether Cmd (⌘) also spells this chord. Terminal chords are Ctrl-only. */
  meta: boolean;
}

/**
 * Every chord claimed by a `window`/`document` listener OUTSIDE the
 * terminal's own inline table, plus the one terminal chord that other
 * trees also reach for.
 *
 * There are exactly two chord registries in this app: this one, and the
 * inline `isCtrlShiftChord(e, "<letter>")` calls in
 * `components/terminal/useKeyboardShortcuts.ts` (pinned by
 * `useKeyboardShortcuts.chords.test.ts`). `globalChords.enforcement.test.ts`
 * scans the source tree and fails when a chord is claimed from anywhere
 * else, or when a claim is hand-rolled instead of routed through the
 * predicates below. Splitting the letters across more than two places is
 * how the same collision came back three times.
 */
export const GLOBAL_CHORDS = {
  /** Terminal command palette (`useKeyboardShortcuts` → `TOGGLE_COMMAND_PALETTE`). */
  commandPalette: { key: "k", shift: true, meta: false },
  /** Productivity knowledge browser (`useKnowledgeBrowserHotkey`). */
  knowledgeBrowser: { key: "e", shift: true, meta: false },
  /** Unified search modal (`unified-search/CommandPalette`) — Cmd/Ctrl+K, NO Shift. */
  unifiedSearch: { key: "k", shift: false, meta: true },
  /** Dev performance overlay (`dev/PerformanceOverlay`), mounted app-wide. */
  performanceOverlay: { key: "p", shift: true, meta: false },
  /** Dev giant-SCC fixture (`dev/GiantSCCFixture`), mounted app-wide. */
  sccFixture: { key: "g", shift: true, meta: false },
  /** Active-dashboard refresh (`active-dashboard/DashboardPage`). */
  dashboardRefresh: { key: "r", shift: false, meta: true },
  /** HTML capture viewer search focus (`dom-captures/HtmlViewerModal`). */
  htmlViewerSearch: { key: "f", shift: false, meta: false },
} as const satisfies Record<string, GlobalChord>;

/**
 * True when the event spells `chord`, case-insensitively.
 *
 * Shift is matched EXACTLY — a chord declared `shift: false` does not
 * fire while Shift is held, which is what stops `Ctrl+Shift+K` from
 * also opening the `Ctrl+K` surface. Alt is deliberately not inspected:
 * none of the chords above are Alt-qualified, and the inline terminal
 * handlers don't test it either; checking it here alone would make the
 * two disagree.
 */
export function matchesChord(e: ChordKeyLike, chord: GlobalChord): boolean {
  if (!(e.ctrlKey || (chord.meta && e.metaKey === true))) return false;
  if (e.shiftKey !== chord.shift) return false;
  return e.key.toLowerCase() === chord.key.toLowerCase();
}

/**
 * True when the event is `Ctrl+Shift+<letter>`, case-insensitively.
 *
 * The spelling used by the terminal's inline chords, which are named by
 * a bare letter rather than a table entry.
 */
export function isCtrlShiftChord(e: ChordKeyLike, letter: string): boolean {
  return matchesChord(e, { key: letter, shift: true, meta: false });
}
