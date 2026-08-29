/**
 * Makes `GLOBAL_CHORDS` ENFORCEABLE rather than documentary.
 *
 * The same defect has now landed three times: a surface claims a chord
 * with its own hand-rolled `keydown` listener, the chord table never
 * hears about it, and two overlays open at once — most recently
 * `unified-search/CommandPalette`, whose `(e.metaKey || e.ctrlKey) &&
 * e.key === "k"` had no `shiftKey` test at all, so `Ctrl+Shift+k`
 * (CapsLock's lowercase spelling) opened it ON TOP of the terminal
 * palette. Three occurrences is a missing test, not three mistakes.
 *
 * So this file scans the source tree instead of trusting the table. It
 * pins three properties:
 *
 *   A. No global `keydown` handler compares `e.key` to a key literal —
 *      ANY key name, not just a letter — alongside a POSITIVELY asserted
 *      control modifier. Use `matchesChord` / `isCtrlShiftChord`.
 *
 *      This rule started narrower, and the narrowness is what let a
 *      FOURTH occurrence land green. It matched only a single letter,
 *      because CapsLock case-flipping was read as the defect rather than
 *      as one symptom of it. The actual defect is a chord claimed
 *      outside the table, and `Ctrl+Tab` — hand-rolled in BOTH
 *      `terminal/useKeyboardShortcuts` and `active-dashboard/ActiveRunsBar`,
 *      double-firing whenever ≥2 runs were active — carries no letter to
 *      match. Nor does `Ctrl+/` in `terminal/CommandBar`. Properties B
 *      and C could not see either one either: both count claims spelled
 *      as `matchesChord(...)` / `isCtrlShiftChord(...)`, and a hand-rolled
 *      claim contributes nothing to count. So the suite asserted "exactly
 *      two chord registries" against a source tree that had four claim
 *      sites — the assertion was true of what it measured and false of
 *      the code.
 *
 *   B. There are exactly TWO chord registries — this table, and the
 *      inline `isCtrlShiftChord(e, "<letter>")` calls in
 *      `terminal/useKeyboardShortcuts.ts` (pinned separately by
 *      `useKeyboardShortcuts.chords.test.ts`). Any other file that
 *      claims a chord must name a `GLOBAL_CHORDS` entry.
 *
 *   C. The set of chords claimed from more than one file is exactly the
 *      documented one. A NEW shared claim — which is what every one of
 *      the three occurrences was — fails here.
 *
 * `environment: "node"` vitest, so `fs` is available; same precedent as
 * `terminal/useKeyboardShortcuts.chords.test.ts` and
 * `terminal/DocFinderModal.fuzzy.test.ts`.
 */

import { readdirSync, readFileSync, statSync } from "fs";
import { join, relative, resolve } from "path";

import { describe, expect, it } from "vitest";

import { GLOBAL_CHORDS, type GlobalChord } from "./globalChords";

const SRC = resolve(__dirname, "..");

/** The terminal's own inline registry — the one sanctioned second home. */
const TERMINAL_REGISTRY = "components/terminal/useKeyboardShortcuts.ts";

/**
 * Chords claimed by more than one file, and why that is tolerated for
 * now. Both are LIVE collisions found by the same sweep that produced
 * this test: the two dev overlays are mounted app-wide from `App.tsx`,
 * so on the terminal page `Ctrl+Shift+P` toggles the control panel AND
 * the performance overlay, and `Ctrl+Shift+G` cycles the tag filter AND
 * the SCC fixture. Reassigning a documented letter is a product call, so
 * they are pinned here rather than silently changed — but nothing NEW
 * can join them without this test going red.
 */
const KNOWN_SHARED_CHORDS: Record<string, string> = {
  "ctrl+shift+g": "terminal cycle-tag-filter vs. dev/GiantSCCFixture (mounted app-wide)",
  "ctrl+shift+p": "terminal TOGGLE_CONTROL_PANEL vs. dev/PerformanceOverlay (mounted app-wide)",
  "ctrl+shift+tab":
    "terminal focus-prev-zone vs. active-dashboard/ActiveRunsBar prev-run (live while >=2 runs)",
  "ctrl+tab":
    "terminal focus-next-zone vs. active-dashboard/ActiveRunsBar next-run (live while >=2 runs)",
};

/* ── source walk ─────────────────────────────────────────────────────── */

function sourceFiles(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) {
      if (entry === "node_modules") continue;
      out.push(...sourceFiles(full));
      continue;
    }
    if (!/\.tsx?$/.test(entry)) continue;
    if (/\.(test|spec)\.tsx?$/.test(entry)) continue;
    out.push(full);
  }
  return out;
}

const FILES = sourceFiles(SRC).map((path) => ({
  rel: relative(SRC, path).split("\\").join("/"),
  source: readFileSync(path, "utf8"),
}));

/** Files that attach a key listener to `window` or `document`. */
const GLOBAL_LISTENER_FILES = FILES.filter((f) =>
  /\b(window|document)\.addEventListener\(\s*"key(down|up|press)"/.test(f.source),
);

/** Every `if (...)` condition in a source file, paren-matched. */
function conditions(source: string): string[] {
  const out: string[] = [];
  const re = /\bif\s*\(/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(source)) !== null) {
    let depth = 1;
    let i = m.index + m[0].length;
    for (; i < source.length && depth > 0; i++) {
      if (source[i] === "(") depth++;
      else if (source[i] === ")") depth--;
    }
    out.push(source.slice(m.index + m[0].length, i - 1));
  }
  return out;
}

/* ── A. no hand-rolled modifier+key comparison ───────────────────────── */

/**
 * True when `cond` POSITIVELY asserts a control modifier — `e.ctrlKey`,
 * not `!e.ctrlKey`.
 *
 * The distinction is load-bearing. `active-dashboard/DashboardPage` tests
 * `e.key === "?" && !e.ctrlKey && !e.metaKey && !e.altKey`: that is a
 * claim on a BARE key, deliberately excluding the modifiers, and it
 * cannot be expressed by {@link matchesChord} at all (every table entry
 * requires Ctrl). Flagging it would demand a rewrite into a predicate
 * that has no way to represent it — a rule the code could not satisfy.
 *
 * Deliberately textual, so it reads a parenthesised negation
 * (`!(e.ctrlKey)`) as POSITIVE. That direction is the safe one — it can
 * only over-report, never wave a real claim through — and no source in
 * the tree spells it that way; the fixtures below pin the forms that
 * exist.
 */
function assertsControlModifier(cond: string): boolean {
  for (const m of cond.matchAll(/(!?)\s*[\w$]+\.(ctrlKey|metaKey)\b/g)) {
    if (m[1] === "") return true;
  }
  return false;
}

/**
 * True when `cond` is a hand-rolled global chord claim: a positively
 * modifier-qualified equality test against a `KeyboardEvent.key` literal.
 *
 * WIDENED from the original single-letter rule (`.key === "<one letter>"`).
 * That rule was written when CapsLock case-flipping was believed to be the
 * defect, so it only looked at the keys CapsLock re-cases. But the defect
 * is a chord claimed OUTSIDE the table, and chords are not only letters:
 * a hand-rolled `e.key === "Tab"` sat live in BOTH
 * `terminal/useKeyboardShortcuts` and `active-dashboard/ActiveRunsBar`
 * — a two-file collision on Ctrl+Tab and Ctrl+Shift+Tab — with this suite
 * green, because neither the letter rule (A) nor the `matchesChord(...)` /
 * `isCtrlShiftChord(...)` claim counters (B, C) can see a claim spelled by
 * hand. `Ctrl+/` in `terminal/CommandBar` was invisible for the same
 * reason. Any key NAME now counts.
 *
 * Exported shape kept as a plain function so the fixtures below can prove
 * the predicate discriminates — see "the offender rule can actually fail".
 */
function isHandRolledChordClaim(cond: string): boolean {
  if (!assertsControlModifier(cond)) return false;
  return /\.key\s*===\s*["'][^"']+["']/.test(cond);
}

describe("global chord handlers", () => {
  it("the offender rule can actually fail", () => {
    // A scanner nobody has watched fail is not a scanner. These fixtures
    // pin BOTH directions, so widening or narrowing the rule by accident
    // shows up here rather than as a silently-green source scan.
    expect(isHandRolledChordClaim('e.ctrlKey && e.key === "Tab"')).toBe(true);
    expect(isHandRolledChordClaim('e.ctrlKey && e.shiftKey && e.key === "Tab"')).toBe(true);
    expect(isHandRolledChordClaim('(e.metaKey || e.ctrlKey) && e.key === "k"')).toBe(true);
    expect(isHandRolledChordClaim('e.ctrlKey && !e.shiftKey && e.key === "/"')).toBe(true);
    expect(isHandRolledChordClaim('e.ctrlKey && (e.key === "Tab" || e.key === "`")')).toBe(true);

    // Bare-key claim that EXCLUDES the modifiers — not expressible as a
    // GLOBAL_CHORDS entry, so not an offender.
    expect(isHandRolledChordClaim('e.key === "?" && !e.ctrlKey && !e.metaKey')).toBe(false);
    // Range test, not an equality against a key literal (the digit
    // layout shortcuts).
    expect(isHandRolledChordClaim('e.ctrlKey && e.key >= "1" && e.key <= "8"')).toBe(false);
    // The sanctioned spellings.
    expect(isHandRolledChordClaim("matchesChord(e, GLOBAL_CHORDS.commandBar)")).toBe(false);
    expect(isHandRolledChordClaim('isCtrlShiftChord(e, "t")')).toBe(false);
  });

  it("never compares e.key to a key literal next to a positive modifier", () => {
    const offenders: string[] = [];
    for (const file of GLOBAL_LISTENER_FILES) {
      for (const cond of conditions(file.source)) {
        if (!isHandRolledChordClaim(cond)) continue;
        offenders.push(`${file.rel}: if (${cond.replace(/\s+/g, " ").trim()})`);
      }
    }
    // Every one of these is a chord claim the table cannot see: it is
    // dead under CapsLock when the literal is a letter, and invisible to
    // the shared-claim counters below whatever the literal is.
    expect(offenders).toEqual([]);
  });
});

/* ── chord claims, extracted from source ─────────────────────────────── */

const spelling = (c: GlobalChord) => `ctrl+${c.shift ? "shift+" : ""}${c.key.toLowerCase()}`;

interface Claim {
  rel: string;
  spelling: string;
  viaTable: boolean;
}

const TABLE_BY_NAME: Record<string, GlobalChord> = GLOBAL_CHORDS;

function claimsIn(rel: string, source: string): Claim[] {
  const out: Claim[] = [];
  for (const m of source.matchAll(/matchesChord\(\s*\w+\s*,\s*GLOBAL_CHORDS\.(\w+)\s*\)/g)) {
    const chord = TABLE_BY_NAME[m[1]];
    expect(chord, `GLOBAL_CHORDS.${m[1]} is referenced by ${rel} but absent`).toBeDefined();
    out.push({ rel, spelling: spelling(chord), viaTable: true });
  }
  for (const m of source.matchAll(
    /matchesChord\(\s*\w+\s*,\s*\{\s*key:\s*"([^"]+)"\s*,\s*shift:\s*(true|false)/g,
  )) {
    out.push({
      rel,
      spelling: spelling({ key: m[1], shift: m[2] === "true", meta: false }),
      viaTable: false,
    });
  }
  for (const m of source.matchAll(/isCtrlShiftChord\(\s*\w+\s*,\s*"([^"]+)"\s*\)/g)) {
    out.push({
      rel,
      spelling: spelling({ key: m[1], shift: true, meta: false }),
      viaTable: false,
    });
  }
  return out;
}

// The chord module itself only MENTIONS the call shapes in its
// docstring; it is the table, not a claimant.
const CLAIMS = FILES.filter((f) => f.rel !== "lib/globalChords.ts").flatMap((f) =>
  claimsIn(f.rel, f.source),
);

describe("chord registries", () => {
  it("finds the claims it is meant to police", () => {
    // Guards against the scanner silently matching nothing — a green
    // test that inspects an empty set is the failure mode this whole
    // file exists to avoid.
    expect(CLAIMS.length).toBeGreaterThan(20);
    expect(GLOBAL_LISTENER_FILES.length).toBeGreaterThan(10);
  });

  it("keeps every non-terminal chord claim in GLOBAL_CHORDS", () => {
    const strays = CLAIMS.filter((c) => !c.viaTable && c.rel !== TERMINAL_REGISTRY).map(
      (c) => `${c.rel} claims ${c.spelling} outside the table`,
    );
    expect(strays).toEqual([]);
  });

  it("assigns a distinct spelling to every table entry", () => {
    const spellings = Object.values(GLOBAL_CHORDS).map(spelling);
    expect(new Set(spellings).size).toBe(spellings.length);
  });

  it("has exactly the documented set of chords claimed by two files", () => {
    const byChord = new Map<string, Set<string>>();
    for (const c of CLAIMS) {
      const files = byChord.get(c.spelling) ?? new Set<string>();
      files.add(c.rel);
      byChord.set(c.spelling, files);
    }
    const shared = [...byChord.entries()]
      .filter(([, files]) => files.size > 1)
      .map(([chord]) => chord)
      .sort();
    expect(shared).toEqual(Object.keys(KNOWN_SHARED_CHORDS).sort());
  });
});
