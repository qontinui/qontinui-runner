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
 *   A. No global `keydown` handler compares `e.key` to a single-letter
 *      literal alongside a control modifier. Letters are the only keys
 *      CapsLock re-cases, so a literal letter comparison is the exact
 *      shape of the recurring bug. Use `matchesChord` /
 *      `isCtrlShiftChord`, which lowercase both sides.
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

/* ── A. no hand-rolled modifier+letter comparison ────────────────────── */

describe("global chord handlers", () => {
  it("never compares e.key to a single-letter literal next to a modifier", () => {
    const offenders: string[] = [];
    for (const file of GLOBAL_LISTENER_FILES) {
      for (const cond of conditions(file.source)) {
        if (!/\b(ctrlKey|metaKey)\b/.test(cond)) continue;
        if (!/\.key\s*===\s*"[A-Za-z]"/.test(cond)) continue;
        offenders.push(`${file.rel}: if (${cond.replace(/\s+/g, " ").trim()})`);
      }
    }
    // A literal letter is dead under CapsLock, which inverts the case
    // Shift produces — the bug behind all three occurrences.
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
