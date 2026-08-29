/**
 * Makes `GLOBAL_CHORDS` ENFORCEABLE rather than documentary.
 *
 * The same defect has now landed five times: a surface claims a chord
 * with its own hand-rolled `keydown` listener, the chord table never
 * hears about it, and two handlers fire on one press. Five occurrences
 * is a missing test, not five mistakes.
 *
 * Occurrence FIVE landed inside the fix for occurrence four, which is
 * what this rewrite is about. The previous scanner recognised exactly
 * ONE spelling of a hand-rolled claim — `e.key === "<literal>"` next to
 * a positive `ctrlKey`/`metaKey` — and a mutation sweep proved it blind
 * to seven others that are all live JavaScript:
 *
 *     e.key.toLowerCase() === "z"     `.toLowerCase()` broke the regex
 *     e.code === "KeyZ"               `.code` is a second key property
 *     const hit = …; if (hit)         claim not inside an `if (...)`
 *     if (e.ctrlKey) switch (e.key)   claim in a `switch` discriminant
 *     ["Z"].includes(e.key)           membership, not equality
 *     "Z" === e.key                   Yoda comparison
 *     e.altKey && e.key === "Z"       Alt was not a modifier at all
 *
 * Worse than blind: its own "the offender rule can actually fail"
 * fixture asserted `isHandRolledChordClaim('e.ctrlKey && e.key >= "1"
 * && e.key <= "8"') === false` — deliberately WHITELISTING the range
 * spelling, which is the exact spelling of the live `Ctrl+1..8`
 * double-fire between `active-dashboard/DashboardPage` and
 * `terminal/useKeyboardShortcuts` that iteration 6 then found on the
 * page. A fixture that blesses a live defect is worse than no fixture:
 * it converts "we never looked" into "we looked and it is fine".
 *
 * `.toLowerCase()` was never hypothetical either — `TerminalInstance.tsx`
 * ships two of them (lines ~1151 and ~1198).
 *
 * So the scanner is now structural rather than shape-matched. It pins
 * five properties:
 *
 *   A. In a file that attaches a global key listener, no KEY-TEST SITE
 *      sits in a statement that positively asserts a control modifier.
 *      A key-test site is any of: an equality/relational comparison
 *      between `.key`/`.code` (optionally case-folded) and a string
 *      literal, in either operand order; a membership test
 *      (`includes`/`has`/`indexOf`) over `.key`/`.code`; or a `switch`
 *      whose discriminant is `.key`/`.code` and whose body has string
 *      `case` labels. The window scanned around a site is its enclosing
 *      STATEMENT, not its enclosing `if (...)` — which is what makes a
 *      hoisted `const hit = …` visible.
 *
 *   B. There are exactly TWO global chord registries — this table, and
 *      the inline `isCtrlShiftChord(e, "<letter>")` calls in
 *      `terminal/useKeyboardShortcuts.ts` (pinned separately by
 *      `useKeyboardShortcuts.chords.test.ts`). Any other file that
 *      claims a chord must name a `GLOBAL_CHORDS` / `GLOBAL_DIGIT_CHORDS`
 *      entry.
 *
 *   C. The set of chords claimed from more than one file is exactly the
 *      documented one. A digit RANGE is expanded to its individual
 *      `ctrl+<digit>` spellings first, so a range claimed twice reports
 *      as eight shared chords rather than as nothing — which is how the
 *      `Ctrl+1..8` collision stayed invisible while this suite was green.
 *
 *   D. `switch (e.key)` registries in NON-listener files are inventoried
 *      by name. They are legitimate today (bare arrow/Escape/Enter keys,
 *      no modifiers), but a Ctrl arm added inside one is a chord claim,
 *      so the roster is pinned and a new one goes red.
 *
 *   E. Element-scoped key registries — xterm's
 *      `attachCustomKeyEventHandler`, which is not a `window` listener
 *      and so was not even a candidate for A — have their modifier-
 *      qualified key literals inventoried and pinned.
 *
 * `environment: "node"` vitest, so `fs` is available; same precedent as
 * `terminal/useKeyboardShortcuts.chords.test.ts` and
 * `terminal/DocFinderModal.fuzzy.test.ts`.
 */

import { readdirSync, readFileSync, statSync } from "fs";
import { join, relative, resolve } from "path";

import { describe, expect, it } from "vitest";

import {
  GLOBAL_CHORDS,
  GLOBAL_DIGIT_CHORDS,
  type GlobalChord,
  type GlobalDigitChord,
} from "./globalChords";

const SRC = resolve(__dirname, "..");

/** The terminal's own inline registry — the one sanctioned second home. */
const TERMINAL_REGISTRY = "components/terminal/useKeyboardShortcuts.ts";

/**
 * Chords claimed by more than one FILE, and why that is tolerated.
 *
 * "Claimed by two files" is a STATIC property. Two of these are live
 * simultaneous double-fires; the digit range is not, and the difference
 * is recorded here rather than smoothed over, because a reader who
 * cannot tell them apart cannot prioritise them.
 */
const KNOWN_SHARED_CHORDS: Record<string, string> = {
  "ctrl+shift+g":
    "terminal cycle-tag-filter vs. dev/GiantSCCFixture. LIVE double-fire: the fixture " +
    "is deliberately shipped in every build (see its header) and mounted app-wide from " +
    "App.tsx, so on the terminal page one press does both. Reassigning a documented " +
    "letter is a product call.",
  "ctrl+shift+p":
    "terminal TOGGLE_CONTROL_PANEL vs. dev/PerformanceOverlay. The overlay's LISTENER is " +
    "now gated on `import.meta.env.DEV` (it used to be gated only at render, so the " +
    "production bundle kept a dead surface's chord claim and its 1 Hz interval alive), " +
    "so production has exactly one claimant. The static two-file claim remains.",
  "ctrl+shift+tab":
    "terminal focus-prev-zone vs. active-dashboard/ActiveRunsBar prev-run (live while >=2 runs)",
  "ctrl+tab":
    "terminal focus-next-zone vs. active-dashboard/ActiveRunsBar next-run (live while >=2 runs)",
  // Ctrl+1..8 — DashboardPage's widget-by-position vs. the terminal's
  // focus-zone-by-number. This WAS a live double-fire (one Ctrl+3 on the
  // Active dashboard moved the terminal's focused zone), and it was
  // invisible to this suite twice over: the range spelling produced no
  // countable claim, and the fixture above explicitly whitelisted it.
  // It is no longer live in either direction — `TabContent` mounts
  // `DashboardPage` only on the Active tab, and the terminal's listener
  // is now inert while its surface is hidden (`isSurfaceVisible`) — but
  // both claims still exist in source, so they are pinned here.
  ...Object.fromEntries(
    [1, 2, 3, 4, 5, 6, 7, 8].map((d) => [
      `ctrl+${d}`,
      "active-dashboard/DashboardPage widget-by-position vs. terminal focus-zone-by-number. " +
        "Not simultaneously live: DashboardPage mounts only on the Active tab and the " +
        "terminal's window listener is surface-visibility gated.",
    ]),
  ),
};

/**
 * Every `switch` on a key property in the tree — the construct a
 * shape-matching scanner cannot see at all.
 *
 * All four dispatch on BARE keys today (arrows, Escape, Enter, Home/End),
 * so none is a chord claim and none can collide with `GLOBAL_CHORDS`. A
 * `case` arm that started testing `event.ctrlKey` WOULD be a claim, and
 * the old scanner — which read only `if (...)` conditions and only a
 * bare `e.key === "<letter>"` — could never have reported it.
 *
 * Two mechanisms cover them now. Property A treats a key `switch` as one
 * site whose window spans the ENTIRE statement including the body, so a
 * modifier test anywhere inside (or an `if (e.ctrlKey)` in front of it)
 * fails — for the ones that are global-listener files. This roster is
 * the second mechanism, and it covers all four: a new key-dispatch
 * registry anywhere in `src/` fails property D and lands in review.
 */
const KNOWN_SWITCH_KEY_REGISTRIES: string[] = [
  "components/PromptSnippetSelector.tsx",
  "components/active-dashboard/ActiveRunsBar.tsx",
  "components/navigation/Sidebar.tsx",
  "hooks/useTutorialKeyboard.ts",
];

/**
 * Element-scoped key registries: handlers attached to a single element
 * rather than to `window`/`document`.
 *
 * `TerminalInstance` registers a THIRD chord registry through xterm's
 * `attachCustomKeyEventHandler`. It is not a `window` listener, so it
 * was not even a candidate for property A — the scanner could not have
 * flagged it however wide the offender rule got.
 *
 * It is deliberately NOT routed through `GLOBAL_CHORDS`. Its scope is
 * different in kind: it fires only while the xterm canvas has focus, and
 * its claims are PTY passthrough semantics (Ctrl+C is copy-or-SIGINT,
 * Ctrl+V is paste-into-PTY) rather than app chords. Forcing them into a
 * window-scoped table would assert a collision that does not exist —
 * its `ctrl+f` and `HtmlViewerModal`'s `ctrl+f` cannot both be focused.
 *
 * What IS enforceable, and is enforced here, is that the set does not
 * grow silently. Property E pins the key literals each such handler
 * tests next to a positive modifier.
 */
const KNOWN_ELEMENT_SCOPED_CLAIMS: Record<string, string[]> = {
  "components/terminal/TerminalInstance.tsx": ["c", "f", "v"],
  "components/terminal/backends/GhosttyBackend.ts": [],
  "components/terminal/backends/XtermBackend.ts": [],
  "components/terminal/backends/types.ts": [],
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

/**
 * Blank out comments, preserving every character offset.
 *
 * Not cosmetic: these modules DOCUMENT the defective spellings they
 * replaced (`unified-search/CommandPalette`'s header quotes its own old
 * `(e.metaKey || e.ctrlKey) && e.key === "k"` verbatim), so a scanner
 * that reads comments reports the fix as the bug. Replacing with spaces
 * rather than deleting keeps `statementWindow`'s offsets honest.
 */
function stripComments(source: string): string {
  const blank = (text: string) => text.replace(/[^\r\n]/g, " ");
  return source
    .replace(/\/\*[\s\S]*?\*\//g, blank)
    .replace(
      /(^|[^:\\])\/\/[^\r\n]*/g,
      (m: string, lead: string) => lead + blank(m.slice(lead.length)),
    );
}

const FILES = sourceFiles(SRC).map((path) => ({
  rel: relative(SRC, path).split("\\").join("/"),
  source: stripComments(readFileSync(path, "utf8")),
}));

/** Files that attach a key listener to `window` or `document`. */
const GLOBAL_LISTENER_FILES = FILES.filter((f) =>
  /\b(window|document)\.addEventListener\(\s*"key(down|up|press)"/.test(f.source),
);

/** Files that register an ELEMENT-scoped key handler (xterm). */
const ELEMENT_SCOPED_FILES = FILES.filter((f) =>
  /\battachCustomKeyEventHandler\s*\(/.test(f.source),
);

/* ── structural scanning primitives ──────────────────────────────────── */

/**
 * The offset just past the balanced closer that opens at `open`.
 * `source[open]` must be the opening bracket.
 */
function matchBracket(source: string, open: number): number {
  const pairs: Record<string, string> = { "(": ")", "{": "}", "[": "]" };
  const closer = pairs[source[open]];
  if (!closer) return open + 1;
  let depth = 0;
  for (let i = open; i < source.length; i++) {
    const ch = source[i];
    if (ch === source[open]) depth++;
    else if (ch === closer) {
      depth--;
      if (depth === 0) return i + 1;
    }
  }
  return source.length;
}

/** Statement boundaries. A statement is what sits between two of these. */
const BOUNDARY = new Set([";", "{", "}"]);

/**
 * The enclosing STATEMENT text for a site spanning `[start, end)`.
 *
 * Scanning to a statement boundary rather than to an enclosing `if (...)`
 * is the whole point: `const hit = e.ctrlKey && e.key === "Z"; if (hit)`
 * has no modifier inside any `if` condition, and the old scanner —
 * which only ever read `if (...)` conditions — was structurally unable
 * to see it. It is also what makes `if (e.ctrlKey) switch (e.key)` and
 * an unbraced `if (e.ctrlKey) doThing(e.key === "Z")` visible, since
 * neither puts a boundary between the modifier and the key test.
 */
function statementWindow(source: string, start: number, end: number): string {
  let from = start;
  while (from > 0 && !BOUNDARY.has(source[from - 1])) from--;
  let to = end;
  while (to < source.length && !BOUNDARY.has(source[to])) to++;
  return source.slice(from, to);
}

interface KeyTestSite {
  /** Offset of the site in the (comment-stripped) source. */
  start: number;
  /** Offset just past the site. */
  end: number;
  /** Statement text scanned for a modifier assertion. */
  window: string;
  /** The comparison / switch header itself, for the failure message. */
  text: string;
  /** String literals the site compares `.key`/`.code` against. */
  literals: string[];
}

/** `.key` / `.code`, optionally case-folded. */
const KEY_PROP = String.raw`\.\s*(?:key|code)\s*(?:\.\s*to(?:Lower|Upper)Case\s*\(\s*\))?`;
const STR = String.raw`(["'\`])((?:(?!\1)[^\\])*?)\1`;
const CMP = String.raw`(?:===|==|!==|!=|>=|<=)`;

/**
 * Every key-test site in `source`.
 *
 * Deliberately over-inclusive on WHAT counts as a key test (any `.key`
 * or `.code`, whatever the receiver) and strict on the modifier check
 * that follows. Over-reporting can only demand that a real chord be
 * routed through the table; under-reporting is how five occurrences of
 * the same defect shipped green.
 */
function keyTestSites(source: string): KeyTestSite[] {
  const out: KeyTestSite[] = [];
  const push = (start: number, end: number, literals: string[]) => {
    out.push({
      start,
      end,
      window: statementWindow(source, start, end),
      text: source.slice(start, end).replace(/\s+/g, " ").trim(),
      literals,
    });
  };

  // `<expr>.key === "X"` (and `>=` / `<=` range comparisons).
  for (const m of source.matchAll(
    new RegExp(KEY_PROP + String.raw`\s*` + CMP + String.raw`\s*` + STR, "g"),
  )) {
    push(m.index, m.index + m[0].length, [m[2]]);
  }
  // Yoda: `"X" === <expr>.key`.
  for (const m of source.matchAll(
    new RegExp(STR + String.raw`\s*` + CMP + String.raw`\s*[\w$.]*` + KEY_PROP, "g"),
  )) {
    push(m.index, m.index + m[0].length, [m[2]]);
  }
  // Membership: `["X"].includes(e.key)`, `SET.has(e.code)`, `.indexOf(e.key)`.
  for (const m of source.matchAll(
    new RegExp(
      String.raw`\.\s*(?:includes|has|indexOf|lastIndexOf)\s*\(\s*[\w$.]*` +
        KEY_PROP +
        String.raw`\s*\)`,
      "g",
    ),
  )) {
    const start = statementStart(source, m.index);
    push(
      m.index,
      m.index + m[0].length,
      stringLiterals(source.slice(start, m.index + m[0].length)),
    );
  }
  // `switch (<expr>.key) { case "X": … }` — window spans the whole
  // statement INCLUDING the body, so a Ctrl arm inside a case is seen
  // and so is an `if (e.ctrlKey)` in front of the `switch`.
  for (const m of source.matchAll(/\bswitch\s*\(/g)) {
    const parenOpen = m.index + m[0].length - 1;
    const parenEnd = matchBracket(source, parenOpen);
    const disc = source.slice(parenOpen, parenEnd);
    if (!new RegExp(KEY_PROP).test(disc)) continue;
    const braceOpen = source.indexOf("{", parenEnd);
    if (braceOpen === -1) continue;
    const braceEnd = matchBracket(source, braceOpen);
    const body = source.slice(braceOpen, braceEnd);
    const cases = [...body.matchAll(new RegExp(String.raw`\bcase\s+` + STR, "g"))].map((c) => c[2]);
    if (cases.length === 0) continue;
    let from = m.index;
    while (from > 0 && !BOUNDARY.has(source[from - 1])) from--;
    out.push({
      start: m.index,
      end: braceEnd,
      window: source.slice(from, braceEnd),
      text: `switch ${disc.replace(/\s+/g, " ")}`,
      literals: cases,
    });
  }
  return out;
}

function statementStart(source: string, at: number): number {
  let from = at;
  while (from > 0 && !BOUNDARY.has(source[from - 1])) from--;
  return from;
}

function stringLiterals(text: string): string[] {
  return [...text.matchAll(new RegExp(STR, "g"))].map((m) => m[2]);
}

/**
 * Names bound to an expression that positively asserts a control
 * modifier — `const mod = e.ctrlKey || e.metaKey;`.
 *
 * Without this, hoisting the modifier half out of the condition hides a
 * claim just as effectively as hoisting the key half did. Cheap, and it
 * can only over-report.
 */
function modifierAliases(source: string): Set<string> {
  const out = new Set<string>();
  // The initialiser is read to end-of-LINE, not to the next `;`. Reading
  // to `;` looked more correct and was strictly worse: `const handler =
  // (e: KeyboardEvent) => {` carries no `;` of its own, so a greedy match
  // swallowed the whole handler body — binding the alias to `handler` and
  // CONSUMING the `const mod = e.ctrlKey || e.metaKey;` inside it, which
  // is the declaration this function exists to find. The mutation matrix
  // caught exactly that: the alias-hoist spelling scanned GREEN against a
  // real file while passing as a snippet fixture.
  for (const m of source.matchAll(/\b(?:const|let|var)\s+([\w$]+)\s*=\s*([^;\r\n]*)/g)) {
    if (rawAssertsControlModifier(m[2], new Set())) out.add(m[1]);
  }
  return out;
}

/**
 * True when `text` POSITIVELY asserts a control modifier — `e.ctrlKey`,
 * not `!e.ctrlKey`.
 *
 * The distinction is load-bearing. `active-dashboard/DashboardPage`
 * tests `e.key === "?" && !e.ctrlKey && !e.metaKey && !e.altKey`: that
 * is a claim on a BARE key, deliberately excluding the modifiers, and it
 * cannot be expressed by `matchesChord` at all (every table entry
 * requires Ctrl). Flagging it would demand a rewrite into a predicate
 * that has no way to represent it.
 *
 * `altKey` counts. It did not before, so `e.altKey && e.key === "Z"` —
 * a perfectly ordinary chord — scanned clean.
 *
 * Deliberately textual, so a parenthesised negation (`!(e.ctrlKey)`)
 * reads as POSITIVE. That direction is the safe one.
 */
function rawAssertsControlModifier(text: string, aliases: Set<string>): boolean {
  for (const m of text.matchAll(/(!?)\s*[\w$]+\.(ctrlKey|metaKey|altKey)\b/g)) {
    if (m[1] === "") return true;
  }
  for (const alias of aliases) {
    if (new RegExp(String.raw`(^|[^.\w$!])` + alias + String.raw`\b`).test(text)) return true;
  }
  return false;
}

/**
 * `[start, end)` of every statement GOVERNED by an `if (...)` whose
 * condition positively asserts a control modifier.
 *
 * The statement window alone is not enough, and the mutation matrix is
 * what proved it: `if (e.ctrlKey) { switch (e.key) { case "Z": … } }`
 * scanned GREEN, because the `{` opening the guarded block IS a
 * statement boundary — so the window around the `switch` stopped short
 * of the very condition that makes it a chord claim. A key test anywhere
 * inside a modifier-guarded block is a claim on that modifier.
 */
function modifierGuardedRanges(source: string, aliases: Set<string>): Array<[number, number]> {
  const out: Array<[number, number]> = [];
  for (const m of source.matchAll(/\bif\s*\(/g)) {
    const open = m.index + m[0].length - 1;
    const close = matchBracket(source, open);
    if (!rawAssertsControlModifier(source.slice(open, close), aliases)) continue;
    let i = close;
    while (i < source.length && /\s/.test(source[i])) i++;
    if (source[i] === "{") {
      out.push([close, matchBracket(source, i)]);
      continue;
    }
    // Unbraced arm: to the next `;` at depth 0, or to the enclosing close.
    let depth = 0;
    let j = i;
    for (; j < source.length; j++) {
      const ch = source[j];
      if ("({[".includes(ch)) depth++;
      else if (")}]".includes(ch)) {
        if (depth === 0) break;
        depth--;
      } else if (ch === ";" && depth === 0) {
        j++;
        break;
      }
    }
    out.push([close, j]);
  }
  return out;
}

/**
 * True when `site` is a hand-rolled chord claim: a key test sitting in a
 * statement that positively asserts a control modifier, or anywhere
 * inside a block guarded by one.
 */
function isHandRolledChordClaim(
  site: KeyTestSite,
  aliases: Set<string> = new Set(),
  guards: Array<[number, number]> = [],
): boolean {
  if (rawAssertsControlModifier(site.window, aliases)) return true;
  return guards.some(([from, to]) => site.start >= from && site.end <= to);
}

/** Convenience for the fixtures: scan a snippet as if it were a file. */
function claimsInSnippet(snippet: string): KeyTestSite[] {
  const aliases = modifierAliases(snippet);
  const guards = modifierGuardedRanges(snippet, aliases);
  return keyTestSites(snippet).filter((s) => isHandRolledChordClaim(s, aliases, guards));
}

/* ── A. no hand-rolled modifier+key test ─────────────────────────────── */

describe("global chord handlers", () => {
  it("the offender rule can actually fail", () => {
    // A scanner nobody has watched fail is not a scanner. Every spelling
    // below was mutation-tested against a real global-listener file; the
    // seven marked GREEN were the ones the previous rule let through.
    const offenders = [
      ['e.ctrlKey && e.key === "Z"', "was RED"],
      ['e.ctrlKey && !e.shiftKey && e.key === "/"', "was RED"],
      ['e.ctrlKey && e.key.toLowerCase() === "z"', "was GREEN — .toLowerCase()"],
      ['e.ctrlKey && e.code === "KeyZ"', "was GREEN — .code"],
      ['const hit = e.ctrlKey && e.key === "Z"; if (hit) { act(); }', "was GREEN — hoisted"],
      ['if (e.ctrlKey) switch (e.key) { case "Z": act(); }', "was GREEN — switch registry"],
      [
        'if (e.ctrlKey) { switch (e.key) { case "Z": act(); } }',
        "BRACED guarded switch — GREEN even after the first rewrite",
      ],
      ['if (e.metaKey) { switch (e.code) { case "KeyZ": act(); } }', "guarded switch on e.code"],
      ['if (e.ctrlKey) { if (e.key === "Z") act(); }', "guarded nested equality"],
      ['e.ctrlKey && ["Z"].includes(e.key)', "was GREEN — membership"],
      ['e.ctrlKey && "Z" === e.key', "was GREEN — Yoda"],
      ['e.altKey && e.key === "Z"', "was GREEN — Alt not a modifier"],
      // The range spelling. The PREVIOUS fixture asserted this was NOT
      // an offender — whitelisting, by literal text, the live Ctrl+1..8
      // double-fire that iteration 6 then found on the page.
      ['e.ctrlKey && e.key >= "1" && e.key <= "8"', "was WHITELISTED by the old fixture"],
      ['(e.metaKey || e.ctrlKey) && e.key === "k"', "was RED"],
      ['e.ctrlKey && e.shiftKey && e.key === "Tab"', "was RED"],
      ['e.ctrlKey && (e.key === "Tab" || e.key === "`")', "was RED"],
      ['const mod = e.ctrlKey || e.metaKey; if (mod && e.key === "z") { act(); }', "alias hoist"],
      ['e.ctrlKey && e.key.toUpperCase() === "Z"', ".toUpperCase()"],
      ["e.ctrlKey && KEYS.has(e.key)", "Set membership"],
    ];
    for (const [snippet, why] of offenders) {
      expect(claimsInSnippet(snippet).length, `${why}: ${snippet}`).toBeGreaterThan(0);
    }

    const clean = [
      // Bare-key claim that EXCLUDES the modifiers — not expressible as a
      // GLOBAL_CHORDS entry, so not an offender.
      'e.key === "?" && !e.ctrlKey && !e.metaKey && !e.altKey',
      'e.key === "Escape"',
      'switch (e.key) { case "ArrowRight": next(); }',
      // The sanctioned spellings.
      "matchesChord(e, GLOBAL_CHORDS.commandBar)",
      'isCtrlShiftChord(e, "t")',
      "matchesDigitChord(e, GLOBAL_DIGIT_CHORDS.terminalFocusZone)",
    ];
    for (const snippet of clean) {
      expect(claimsInSnippet(snippet), snippet).toEqual([]);
    }
  });

  it("finds key-test sites at all", () => {
    // Guards against every regex above silently matching nothing — a
    // green scan of an empty set is the failure mode this file exists
    // to avoid.
    const sites = GLOBAL_LISTENER_FILES.flatMap((f) => keyTestSites(f.source));
    expect(GLOBAL_LISTENER_FILES.length).toBeGreaterThan(10);
    expect(sites.length).toBeGreaterThan(20);
  });

  it("never tests a key literal inside a statement asserting a modifier", () => {
    const offenders: string[] = [];
    for (const file of GLOBAL_LISTENER_FILES) {
      const aliases = modifierAliases(file.source);
      const guards = modifierGuardedRanges(file.source, aliases);
      for (const site of keyTestSites(file.source)) {
        if (!isHandRolledChordClaim(site, aliases, guards)) continue;
        offenders.push(`${file.rel}: ${site.text}`);
      }
    }
    // Every one of these is a chord claim the table cannot see, and
    // which the shared-claim counters below therefore cannot count.
    expect(offenders).toEqual([]);
  });
});

/* ── chord claims, extracted from source ─────────────────────────────── */

const spelling = (c: GlobalChord) => `ctrl+${c.shift ? "shift+" : ""}${c.key.toLowerCase()}`;

/** A digit range expands to one spelling per digit it covers. */
function digitSpellings(c: GlobalDigitChord): string[] {
  const out: string[] = [];
  for (let d = c.from; d <= c.to; d++) out.push(`ctrl+${c.shift ? "shift+" : ""}${d}`);
  return out;
}

interface Claim {
  rel: string;
  spelling: string;
  viaTable: boolean;
}

const TABLE_BY_NAME: Record<string, GlobalChord> = GLOBAL_CHORDS;
const DIGIT_TABLE_BY_NAME: Record<string, GlobalDigitChord> = GLOBAL_DIGIT_CHORDS;

function claimsIn(rel: string, source: string): Claim[] {
  const out: Claim[] = [];
  for (const m of source.matchAll(/matchesChord\(\s*\w+\s*,\s*GLOBAL_CHORDS\.(\w+)\s*\)/g)) {
    const chord = TABLE_BY_NAME[m[1]];
    expect(chord, `GLOBAL_CHORDS.${m[1]} is referenced by ${rel} but absent`).toBeDefined();
    out.push({ rel, spelling: spelling(chord), viaTable: true });
  }
  for (const m of source.matchAll(
    /matchesDigitChord\(\s*\w+\s*,\s*GLOBAL_DIGIT_CHORDS\.(\w+)\s*\)/g,
  )) {
    const chord = DIGIT_TABLE_BY_NAME[m[1]];
    expect(chord, `GLOBAL_DIGIT_CHORDS.${m[1]} is referenced by ${rel} but absent`).toBeDefined();
    for (const s of digitSpellings(chord)) out.push({ rel, spelling: s, viaTable: true });
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
    expect(CLAIMS.length).toBeGreaterThan(20);
    expect(GLOBAL_LISTENER_FILES.length).toBeGreaterThan(10);
    // The digit ranges must actually be reaching the counter — a
    // `matchesDigitChord` call that stopped being recognised would
    // silently restore the exact blind spot this table was added for.
    expect(CLAIMS.filter((c) => /^ctrl\+(shift\+)?\d$/.test(c.spelling).valueOf()).length).toBe(
      8 + 8 + 9,
    );
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

/* ── D + E. the registries property A structurally cannot reach ──────── */

describe("key registries property A cannot police on its own", () => {
  it("has exactly the documented `switch (e.key)` registries", () => {
    // Every `switch` on a key property in the tree, listener file or not.
    // Property A already fails any of these that grows a modifier arm
    // (the switch's window spans its whole body). This roster is the
    // second half: it makes a NEW key-dispatch registry visible in review
    // even while every arm in it is still a bare key.
    const found = FILES.filter((f) =>
      keyTestSites(f.source).some((s) => s.text.startsWith("switch")),
    ).map((f) => f.rel);
    expect(found.sort()).toEqual([...KNOWN_SWITCH_KEY_REGISTRIES].sort());
  });

  it("pins the key literals every element-scoped handler claims with a modifier", () => {
    const found: Record<string, string[]> = {};
    for (const file of ELEMENT_SCOPED_FILES) {
      const aliases = modifierAliases(file.source);
      const guards = modifierGuardedRanges(file.source, aliases);
      const literals = new Set<string>();
      for (const site of keyTestSites(file.source)) {
        if (!isHandRolledChordClaim(site, aliases, guards)) continue;
        for (const lit of site.literals) literals.add(lit.toLowerCase());
      }
      found[file.rel] = [...literals].sort();
    }
    // A new modifier-qualified key in an xterm handler changes this set.
    // That is the only enforcement available for a handler that is not a
    // `window` listener — but it is real enforcement, not a comment.
    expect(found).toEqual(KNOWN_ELEMENT_SCOPED_CLAIMS);
  });
});
