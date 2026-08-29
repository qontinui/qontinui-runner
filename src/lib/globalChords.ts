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
 * A FOURTH round found the scanner itself too narrow to see the thing it
 * was built to see. Its offender rule matched only a single-LETTER
 * literal (`e.key === "k"`), on the theory that letters are what CapsLock
 * re-cases — but `Ctrl+Tab`, `` Ctrl+` `` and `Ctrl+/` are chords too, and
 * a hand-rolled `e.key === "Tab"` was live in BOTH
 * `terminal/useKeyboardShortcuts` and `active-dashboard/ActiveRunsBar`
 * while the enforcement test ran green. The table asserted "exactly two
 * chord registries" and the code had three; the scanner could not see the
 * third because a hand-rolled claim contributes no `matchesChord(...)`
 * text for it to count. CapsLock was never the general defect — it was
 * one symptom of the general defect, which is a chord claimed OUTSIDE
 * this table. The scanner now flags a positively-modifier-qualified
 * `e.key === "<any key name>"`, letter or not.
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
  altKey?: boolean;
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
  /**
   * Cycle FORWARD — Ctrl+Tab. Claimed by TWO surfaces: the terminal's
   * zone/tab walk (`terminal/useKeyboardShortcuts`) and the active
   * dashboard's run carousel (`active-dashboard/ActiveRunsBar`, live
   * whenever ≥2 runs are active). Listed in the enforcement test's
   * `KNOWN_SHARED_CHORDS` for exactly that reason.
   */
  cycleNext: { key: "tab", shift: false, meta: false },
  /** Cycle BACKWARD — Ctrl+Shift+Tab. Same two claimants as {@link cycleNext}. */
  cyclePrev: { key: "tab", shift: true, meta: false },
  /** Second spelling of the dashboard's forward run cycle — Ctrl+`. */
  runCycleAlt: { key: "`", shift: false, meta: false },
  /** Terminal CommandBar focus (`terminal/CommandBar`) — Ctrl+/, no Shift. */
  commandBar: { key: "/", shift: false, meta: false },
} as const satisfies Record<string, GlobalChord>;

/**
 * A chord whose key is a contiguous DIGIT RANGE rather than one key —
 * `Ctrl+1..8`, `Ctrl+Shift+1..8`, `Ctrl+1..9`.
 *
 * These could not live in {@link GLOBAL_CHORDS} because that table is
 * keyed by a single `e.key`, and that is exactly why they stayed
 * hand-rolled as `e.key >= "1" && e.key <= "8"` range comparisons in two
 * files at once — a spelling the enforcement scanner's claim counters
 * could not see, so the collision below was invisible to every test in
 * the suite while it fired live on the page.
 *
 * The scanner expands a range into its individual `ctrl+<digit>`
 * spellings, so a range claimed from two files reports as eight shared
 * chords rather than as nothing.
 */
export interface GlobalDigitChord {
  /** Lowest digit of the range, inclusive. */
  from: number;
  /** Highest digit of the range, inclusive. */
  to: number;
  /** Whether Shift is PART of the chord. `false` means Shift must be ABSENT. */
  shift: boolean;
  /** Whether Cmd (⌘) also spells this chord. */
  meta: boolean;
}

/**
 * Every digit-range chord claimed by a `window`/`document` listener.
 *
 * `dashboardWidget` and `terminalFocusZone` OVERLAP on `Ctrl+1..8`. That
 * was a live two-handler double-fire: `active-dashboard/DashboardPage`
 * and `terminal/useKeyboardShortcuts` both attach to `window`, and
 * `TerminalPage` stays MOUNTED (merely `display:none`) on every other
 * tab — so one `Ctrl+3` pressed on the Active dashboard switched the
 * dashboard widget AND moved the terminal's focused zone. The fix is not
 * to reassign a documented shortcut on either surface: it is that the
 * terminal's listener is now inert while its surface is not visible
 * (`isSurfaceVisible`), and `DashboardPage` only mounts on the Active
 * tab. The static overlap is therefore pinned in the enforcement test's
 * `KNOWN_SHARED_CHORDS` as never-simultaneously-live, not as a share.
 */
export const GLOBAL_DIGIT_CHORDS = {
  /** Active-dashboard widget-by-position (`active-dashboard/DashboardPage`). */
  dashboardWidget: { from: 1, to: 8, shift: false, meta: true },
  /** Terminal layout preset by number (`terminal/useKeyboardShortcuts`). */
  terminalLayoutPreset: { from: 1, to: 8, shift: true, meta: false },
  /** Terminal focus-zone by number (`terminal/useKeyboardShortcuts`). */
  terminalFocusZone: { from: 1, to: 9, shift: false, meta: false },
} as const satisfies Record<string, GlobalDigitChord>;

/**
 * The digit the event spells for `chord`, or `null` when it does not
 * spell it.
 *
 * Shift is matched EXACTLY, same as {@link matchesChord} — which is the
 * half `DashboardPage` was missing entirely. It tested
 * `(e.ctrlKey || e.metaKey) && e.key >= "1" && e.key <= "8"` with NO
 * `shiftKey` term, so `Ctrl+Shift+1..8` had a single claimant only by
 * accident of the US layout (where shifted digits report punctuation).
 * On a numpad or a non-US layout that reports a bare digit with Shift
 * held, it was a second live collision — with the terminal's LAYOUT
 * preset chord, on top of the zone-focus one.
 *
 * Alt IS inspected here, unlike {@link matchesChord}: `Ctrl+Alt+<digit>`
 * is an OS-level chord on several platforms, and the terminal's
 * focus-zone handler has always carried an explicit `!e.altKey` term
 * that would otherwise be dropped by the routing.
 */
export function matchesDigitChord(e: ChordKeyLike, chord: GlobalDigitChord): number | null {
  if (!(e.ctrlKey || (chord.meta && e.metaKey === true))) return null;
  if (e.shiftKey !== chord.shift) return null;
  if (e.altKey === true) return null;
  if (!/^[0-9]$/.test(e.key)) return null;
  const n = Number(e.key);
  return n >= chord.from && n <= chord.to ? n : null;
}

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
