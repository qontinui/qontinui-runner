/**
 * Makes `GLOBAL_CHORDS` ENFORCEABLE rather than documentary.
 *
 * The same defect has now landed six times: a surface claims a chord with
 * its own hand-rolled key test, the chord table never hears about it, and
 * two handlers fire on one press. Six occurrences is a missing mechanism,
 * not six mistakes — and the previous five fixes were all the same
 * mechanism (a regex, widened once more), which is why there were five of
 * them.
 *
 * ## What changed, and why it is not a sixth widening
 *
 * Iteration 7 of the manual-test loop mutation-tested the regex scanner
 * with 27 spellings: 13 red, **14 green**, including one that is live in
 * `src/` today (`const { key, ctrlKey } = e`, `scrollKeys.ts:47`) and one
 * that is the body of `matchesDigitChord` itself (`/^[0-9]$/.test(e.key)`)
 * — the fix for the range defect, written in a spelling the test policing
 * range defects could not see.
 *
 * The scanner is now an AST pass (`./keyClaimScan.ts`) that recognises the
 * **key READ** rather than the comparison spelling, and resolves
 * destructuring, aliasing, bracket access, legacy `keyCode`/`which`,
 * `getModifierState`, hoisted modifiers and early-return guards
 * structurally. The full rationale, and what was rejected (a lint-rule ban
 * on field access; `ts-morph`), is in that module's header.
 *
 * ## The selection bug this also closes
 *
 * The old suite could only look at files it had SELECTED — those matching
 * `addEventListener("key…")` or `attachCustomKeyEventHandler(`. A module
 * that holds a key mapping but registers nothing was invisible however
 * good the offender rule got, and one is live: `terminal/scrollKeys.ts`
 * claims EIGHT chords (Ctrl+Home/End, Ctrl+Up/Down, Ctrl+Alt+PageUp/Down,
 * Shift+PageUp/Down) and was wired in through `TerminalInstance`, whose
 * pinned claim set said `["c", "f", "v"]` while the suite ran green.
 *
 * Selection is therefore by DATA now: property A scans the WHOLE tree and
 * pins every modifier-qualified key claim wherever it lives, registration
 * site or not. That is the inversion the "you cannot claim a chord without
 * going through the table" idea is really after — you may not claim one
 * ANYWHERE unless you route it through `globalChords.ts` or are named,
 * with your exact chord spelling, in {@link KNOWN_KEY_CLAIMS}.
 *
 * ## The five properties
 *
 *   A. Every modifier-qualified key claim in `src/` is exactly the
 *      allowlisted set, per file, spelled as a full chord. A claim inside
 *      a `window`/`document` key listener is additionally forbidden
 *      outright — that one is app-wide and the table exists to own it.
 *   B. There are exactly TWO global chord registries — this table, and the
 *      inline `isCtrlShiftChord(e, "<letter>")` calls in
 *      `terminal/useKeyboardShortcuts.ts` (pinned separately by
 *      `useKeyboardShortcuts.chords.test.ts`).
 *   C. The set of chords claimed from more than one file is exactly the
 *      documented one. A digit RANGE is expanded to its individual
 *      `ctrl+<digit>` spellings first, so a range claimed twice reports as
 *      eight shared chords rather than as nothing — which is how the
 *      `Ctrl+1..8` collision stayed invisible while this suite was green.
 *   D. `switch` registries on a key value are inventoried by name. All
 *      four dispatch on BARE keys today, so none is a chord claim; a new
 *      one lands in review.
 *   E. The scanner can actually fail — the mutation matrix, run both as
 *      snippets and INJECTED INTO A REAL FILE. The real-file arm is not
 *      ceremony: the previous rewrite had a spelling that passed as a
 *      snippet and scanned green against a real file.
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
import { CONTROL_TAGS, scanKeyClaims, type FileScan } from "./keyClaimScan";

const SRC = resolve(__dirname, "..");

/** The terminal's own inline registry — the one sanctioned second home. */
const TERMINAL_REGISTRY = "components/terminal/useKeyboardShortcuts.ts";

/**
 * Files that are the MECHANISM rather than a claimant: the chord table
 * whose predicates do the reading, and the scanner that reads them.
 */
const MECHANISM_FILES = new Set(["lib/globalChords.ts", "lib/keyClaimScan.ts"]);

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
  // countable claim, and a fixture explicitly whitelisted it.
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
 * Every `switch` on a key value in the tree — the construct a
 * shape-matching scanner cannot see at all.
 *
 * All four dispatch on BARE keys today (arrows, Escape, Enter, Home/End),
 * so none is a chord claim and none can collide with `GLOBAL_CHORDS`. A
 * `case` arm that started testing `event.ctrlKey` WOULD be a claim, and
 * property A catches that directly (the switch's discriminant read
 * inherits the modifiers of every enclosing condition and preceding
 * guard). This roster is the second half: it makes a NEW key-dispatch
 * registry visible in review even while every arm in it is a bare key.
 */
const KNOWN_SWITCH_KEY_REGISTRIES: string[] = [
  "components/PromptSnippetSelector.tsx",
  "components/active-dashboard/ActiveRunsBar.tsx",
  "components/navigation/Sidebar.tsx",
  "hooks/useTutorialKeyboard.ts",
];

/**
 * EVERY modifier-qualified key claim in `src/`, by file, spelled as a full
 * chord — the allowlist that makes property A an inversion rather than a
 * search. Anything not routed through `globalChords.ts`'s predicates and
 * not named here fails.
 *
 * A `shift+…` entry is a shift-ONLY claim: `matchesChord` cannot express
 * one (every table entry requires Ctrl), so it is inventoried rather than
 * demanded into the table. See `keyClaimScan.ts::CONTROL_TAGS`.
 *
 * All three files below are ELEMENT-scoped — a focused textarea, and
 * xterm's `attachCustomKeyEventHandler`. That is why they are tolerated
 * outside the table and not merely tolerated silently: their scope is
 * different in kind from a `window` listener's. They fire only while one
 * element has focus, and their claims are passthrough semantics (Ctrl+C is
 * copy-or-SIGINT, Ctrl+V is paste-into-PTY, Ctrl+Home scrolls the
 * scrollback) rather than app chords. Forcing them into a window-scoped
 * table would assert collisions that do not exist — `TerminalInstance`'s
 * `ctrl+f` and `HtmlViewerModal`'s `ctrl+f` cannot both be focused.
 *
 * What IS enforceable, and is enforced here, is that the set does not grow
 * silently. It had already grown by eight before this list could see them.
 */
const KNOWN_KEY_CLAIMS: Record<string, string[]> = {
  // Ctrl/Cmd+Enter submits the prompt from the focused textarea.
  "components/scheduler/AiScheduleBuilder.tsx": ["ctrl+enter"],
  // xterm `attachCustomKeyEventHandler` — clipboard + find, PTY-scoped.
  // Bare F3 / Shift+F3 also handled there; a bare key is not a chord claim.
  "components/terminal/TerminalInstance.tsx": ["ctrl+c", "ctrl+f", "ctrl+shift+c", "ctrl+v"],
  // VS Code-parity scrollback navigation, consumed by `TerminalInstance`'s
  // xterm handler. THE EIGHT CLAIMS THE OLD SCANNER COULD NOT SEE: the file
  // holds no listener of its own, so no selection rule reached it, and its
  // `const { key, shiftKey, ctrlKey, altKey, metaKey } = e` destructure hid
  // every key test from a `.key`-anchored regex.
  "components/terminal/scrollKeys.ts": [
    "ctrl+alt+pagedown",
    "ctrl+alt+pageup",
    "ctrl+arrowdown",
    "ctrl+arrowup",
    "ctrl+end",
    "ctrl+home",
    "shift+pagedown",
    "shift+pageup",
  ],
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

/**
 * Cheap gate on which files are PARSED, and why it is sound rather than a
 * second (regex) scanner smuggled back in.
 *
 * Every construct the AST scanner recognises requires one of these
 * identifiers to appear literally in the text — a property access
 * (`.key`), an element access (`["key"]`), a destructured binding
 * (`{ key }`), or an alias source. A file containing none of these words
 * anywhere cannot contain a key read under any spelling, so skipping it
 * removes no coverage. It is a substring test on FIELD NAMES, never on
 * the shape of a comparison, which is the thing that kept failing.
 */
const COULD_READ_A_KEY =
  /\b(?:key|code|keyCode|which|ctrlKey|metaKey|altKey|shiftKey|getModifierState)\b/;

const SCANS: Array<{ rel: string; scan: FileScan }> = FILES.filter(
  (f) => !MECHANISM_FILES.has(f.rel) && COULD_READ_A_KEY.test(f.source),
).map((f) => ({ rel: f.rel, scan: scanKeyClaims(f.source, f.rel) }));

/**
 * Files that attach a key listener to `window` or `document`.
 *
 * A regex is fine HERE and nowhere else in this file: this matches a
 * REGISTRATION CALL, whose spelling is fixed by the DOM API, not a chord
 * claim, whose spelling is open-ended. It no longer gates what is scanned
 * — property A scans everything — only which claims are graded as
 * app-wide.
 */
const GLOBAL_LISTENER_FILES = new Set(
  FILES.filter((f) =>
    /\b(window|document)\.addEventListener\(\s*"key(down|up|press)"/.test(f.source),
  ).map((f) => f.rel),
);

/** Scan a snippet as if it were a file, keeping only the claims. */
function claimsInSnippet(snippet: string): string[] {
  return scanKeyClaims(snippet, "snippet.ts").claims.map((c) => c.spelling);
}

/**
 * Scan a snippet INJECTED INTO A REAL FILE.
 *
 * The previous rewrite's mutation matrix caught a spelling that passed as
 * a standalone snippet and scanned GREEN inside a real module, because the
 * scanner's alias pass had a whole-file artefact. A snippet fixture alone
 * is therefore not evidence; this is the arm that is.
 */
const HOST_REL = TERMINAL_REGISTRY;
const HOST_SOURCE = FILES.find((f) => f.rel === HOST_REL)?.source ?? "";

function claimsInRealFile(snippet: string): string[] {
  const injected = `${HOST_SOURCE}\nfunction __mutationProbe(e: KeyboardEvent, act: () => void) {\n  ${snippet}\n  void act;\n}\n`;
  return scanKeyClaims(injected, HOST_REL).claims.map((c) => c.spelling);
}

/* ── E. the scanner can actually fail ────────────────────────────────── */

/**
 * The mutation matrix. Every spelling iteration 7 tested, plus the ones
 * earlier rounds tested, with the verdict the REGEX scanner gave.
 *
 * `RED` = the regex caught it. `GREEN` = it walked through — the escape
 * set that made a seventh widening pointless. All of them must be red now.
 */
const OFFENDERS: Array<[string, string]> = [
  // ── caught by the regex scanner (13) ──
  ['e.ctrlKey && e.key === "Z";', "RED — plain equality"],
  ['e.ctrlKey && !e.shiftKey && e.key === "/";', "RED — negated shift term"],
  ['e.ctrlKey && e.key.toLowerCase() === "z";', "RED — case-folded"],
  ['e.ctrlKey && e.code === "KeyZ";', "RED — .code"],
  ['const hit = e.ctrlKey && e.key === "Z"; if (hit) { act(); }', "RED — hoisted key half"],
  ['if (e.ctrlKey) switch (e.key) { case "Z": act(); }', "RED — unbraced guarded switch"],
  ['if (e.ctrlKey) { switch (e.key) { case "Z": act(); } }', "RED — braced guarded switch"],
  ['if (e.metaKey) { switch (e.code) { case "KeyZ": act(); } }', "RED — guarded switch on .code"],
  ['if (e.ctrlKey) { if (e.key === "Z") act(); }', "RED — guarded nested equality"],
  ['e.ctrlKey && ["Z"].includes(e.key);', "RED — array membership"],
  ['e.ctrlKey && "Z" === e.key;', "RED — Yoda"],
  ['e.altKey && e.key === "Z";', "RED — Alt as the modifier"],
  ['e.ctrlKey && e.key >= "1" && e.key <= "8";', "RED — digit range"],

  // ── ALSO caught by the regex scanner, from earlier rounds ──
  ['(e.metaKey || e.ctrlKey) && e.key === "k";', "RED — Cmd-alias idiom"],
  ['e.ctrlKey && e.shiftKey && e.key === "Tab";', "RED — named key"],
  ['e.ctrlKey && (e.key === "Tab" || e.key === "`");', "RED — disjunction of keys"],
  ['const mod = e.ctrlKey || e.metaKey; if (mod && e.key === "z") { act(); }', "RED — alias hoist"],
  ['e.ctrlKey && e.key.toUpperCase() === "Z";', "RED — .toUpperCase()"],
  ["e.ctrlKey && KEYS.has(e.key);", "RED — Set membership"],

  // ── the escape set: GREEN against the regex scanner (14) ──
  ['const { key, ctrlKey } = e; if (ctrlKey && key === "z") { act(); }', "GREEN — destructured"],
  ['const k = e.key; if (e.ctrlKey && k === "z") { act(); }', "GREEN — aliased key"],
  ["e.ctrlKey && /^[1-8]$/.test(e.key);", "GREEN — regex range"],
  ["e.ctrlKey && e.keyCode === 90;", "GREEN — legacy .keyCode"],
  ["e.ctrlKey && e.which === 90;", "GREEN — legacy .which"],
  ['if (isMod(e)) { if (e.key === "z") { act(); } }', "GREEN — modifier behind a helper"],
  ['e.ctrlKey && e["key"] === "z";', "GREEN — bracket access"],
  ['e.getModifierState("Control") && e.key === "z";', "GREEN — getModifierState"],
  ["e.ctrlKey && KEYMAP[e.key];", "GREEN — lookup table, no comparison at all"],
  ['e.ctrlKey && e.key.startsWith("z");', "GREEN — startsWith"],
  ["e.ctrlKey && e.key.match(/^z$/);", "GREEN — String.match"],
  ['e.ctrlKey && e.key.localeCompare("z") === 0;', "GREEN — localeCompare"],
  [
    "if (!(e.ctrlKey || e.metaKey)) return;\n  if (!/^[0-9]$/.test(e.key)) return;\n  act();",
    "GREEN — `matchesDigitChord`'s OWN body: early-return guards, regex range",
  ],
  [
    'const { key, shiftKey, ctrlKey } = e;\n  if (ctrlKey && !shiftKey) { if (key === "Home") act(); }',
    "GREEN — `scrollKeys.ts`'s own live spelling",
  ],

  // ── the AST scanner's OWN escape set: GREEN against it until iteration 8
  //    (D6). Every one is a RECEIVER spelled a way `ts.isIdentifier` could
  //    not see — a pair of parentheses, a non-null assertion, a cast, or one
  //    hop down a chain. Purely syntactic, single-expression, no type
  //    checker involved; the claim was never hidden, only its receiver's
  //    node kind. The asymmetry that made the miss SILENT: `positiveModifiers`
  //    matched ANY property access, so the modifier half was still seen and
  //    only the key half went missing — and a claim needs both.
  ['(e).ctrlKey && (e).key === "z";', "GREEN — parenthesised receiver"],
  ['e!.ctrlKey && e!.key === "z";', "GREEN — non-null-asserted receiver"],
  [
    '(e as KeyboardEvent).ctrlKey && (e as KeyboardEvent).key === "z";',
    "GREEN — cast receiver",
  ],
  ['e.ctrlKey && e.nativeEvent.key === "z";', "GREEN — React's `nativeEvent`, one hop"],
  ['e.nativeEvent.ctrlKey && e.nativeEvent.key === "z";', "GREEN — `nativeEvent` on both halves"],
  ['this.ev.ctrlKey && this.ev.key === "z";', "GREEN — member receiver"],
  ['evs[0].ctrlKey && evs[0].key === "z";', "GREEN — indexed receiver"],
  [
    'const isUndo = (ev: KeyboardEvent) => ev.ctrlKey && ev.key === "z";',
    "GREEN — an arrow's CONCISE body: the modifier and the key read are three " +
      "tokens apart in ONE expression, and it was the only statement-shaped " +
      "position with no arm in `guardModifiers`. The `{ return … }` and ternary " +
      "spellings of the same helper were both RED.",
  ],
  [
    'switch (true) { case e.ctrlKey: if (e.key === "z") act(); }',
    "GREEN — assertion in the CASE CLAUSE, not the discriminant",
  ],
];

/**
 * Spellings that must stay CLEAN. Over-reporting is the safe direction for
 * a scanner, but not without limit: a rule that flags a bare-key handler
 * demands a rewrite into a predicate (`matchesChord`) that cannot express
 * it, so these are load-bearing.
 */
const CLEAN: string[] = [
  // A bare key claimed while DELIBERATELY excluding the modifiers.
  'e.key === "?" && !e.ctrlKey && !e.metaKey && !e.altKey;',
  'e.key === "Escape";',
  'switch (e.key) { case "ArrowRight": next(); }',
  // A guard clause that excludes the modifier — the polarity mirror of the
  // `matchesDigitChord` offender above, and the one that would break if
  // guard inheritance were written without tracking polarity.
  'if (e.ctrlKey) return;\n  if (e.key === "Escape") { act(); }',
  // The sanctioned spellings.
  "matchesChord(e, GLOBAL_CHORDS.commandBar);",
  'isCtrlShiftChord(e, "t");',
  "matchesDigitChord(e, GLOBAL_DIGIT_CHORDS.terminalFocusZone);",
];

describe("the scanner can actually fail", () => {
  it("flags every mutation spelling as a snippet", () => {
    for (const [snippet, why] of OFFENDERS) {
      expect(claimsInSnippet(snippet).length, `${why}: ${snippet}`).toBeGreaterThan(0);
    }
  });

  it("flags every mutation spelling INJECTED INTO A REAL FILE", () => {
    // The host is clean on its own, so any claim comes from the probe.
    expect(scanKeyClaims(HOST_SOURCE, HOST_REL).claims).toEqual([]);
    for (const [snippet, why] of OFFENDERS) {
      expect(claimsInRealFile(snippet).length, `${why}: ${snippet}`).toBeGreaterThan(0);
    }
  });

  it("leaves bare-key and table-routed spellings alone", () => {
    for (const snippet of CLEAN) {
      expect(claimsInSnippet(snippet), snippet).toEqual([]);
      expect(claimsInRealFile(snippet), `in-file: ${snippet}`).toEqual([]);
    }
  });

  /**
   * The limits of THIS mechanism, probed and pinned AS CLASSES.
   *
   * A mechanism whose limits have not been probed is not verified — that
   * is the lesson of the previous five, each of which was believed
   * complete until the next spelling arrived. So the residual escapes are
   * written down rather than discovered later.
   *
   * ## Why classes and not strings
   *
   * The previous version pinned TEN LITERAL SNIPPETS, which cannot go red
   * when the floor MOVES — only when one of those exact strings does. Two
   * spellings one token away from a pinned row were unpinned and behaved
   * differently from nothing at all: `Object.values(e).at(3)` and
   * `MODS.some(…)`. What is pinned here instead is a CLASS plus several
   * spellings OF that class, and the assertion is that every spelling
   * agrees with its class's verdict. A variant that disagrees is either a
   * new escape or a floor that has shrunk, and both have to be written
   * down before this file goes green again. The COUNT of escaping classes
   * is pinned separately, so neither direction can pass silently.
   *
   * ## The framing that was wrong
   *
   * The previous version said its four escapes were "all INTERPROCEDURAL
   * or fully dynamic — closing them needs a type checker or a call graph".
   * That was false of the largest class it had, and the claim is what made
   * the class invisible: a receiver spelled `(e)`, `e!`, `(e as
   * KeyboardEvent)` or `e.nativeEvent` is purely SYNTACTIC and lives in a
   * single expression. It needed a receiver unwrap and a chain-aware rule,
   * not a compiler. So did an arrow's concise body. Both are closed above.
   *
   * What remains is genuinely interprocedural or genuinely nameless, and
   * says so per class.
   */
  interface ProbeClass {
    /** What the class IS — the property, not one of its spellings. */
    name: string;
    /** True when the scanner catches EVERY spelling below. */
    caught: boolean;
    /** Why it escapes. Required when `caught` is false. */
    why?: string;
    /** Several spellings of the SAME class. All must agree with `caught`. */
    spellings: string[];
  }

  const PROBE_CLASSES: ProbeClass[] = [
    {
      name: "event alias chain",
      caught: true,
      spellings: [
        'const t = e; const u = t; if (u.ctrlKey && u.key === "z") act();',
        'const t = e; const u = t; const v = u; if (v.ctrlKey && v.key === "z") act();',
      ],
    },
    {
      name: "key alias chain",
      caught: true,
      spellings: [
        'const k2 = e.key; const k3 = k2; if (e.ctrlKey && k3 === "z") act();',
        'const k2 = e.key; if (e.ctrlKey && k2 === "z") act();',
      ],
    },
    {
      name: "key test in a closure under a modifier guard",
      caught: true,
      spellings: [
        'if (e.ctrlKey) { const f = () => { if (e.key === "z") act(); }; f(); }',
        'if (e.ctrlKey) { [1].forEach(() => { if (e.key === "z") act(); }); }',
      ],
    },
    {
      name: "dynamic field name",
      caught: true,
      spellings: [
        'const F = "key"; e.ctrlKey && e[F] === "z";',
        'const FIELDS = ["key"]; e.ctrlKey && e[FIELDS[0]] === "z";',
      ],
    },
    {
      name: "Reflect.get",
      caught: true,
      spellings: [
        'e.ctrlKey && Reflect.get(e, "key") === "z";',
        'const F2 = "key"; e.ctrlKey && Reflect.get(e, F2) === "z";',
      ],
    },
    {
      name: "computed / renamed binding property",
      caught: true,
      spellings: [
        'const { ["key"]: k } = e; e.ctrlKey && k === "z";',
        'const { key: k, ctrlKey: c } = e; if (c && k === "z") act();',
      ],
    },
    {
      // Closed by D6 of manual-test-loop iteration 8.
      name: "receiver spelled around the identifier test",
      caught: true,
      spellings: [
        '(e).ctrlKey && (e).key === "z";',
        'e!.ctrlKey && e!.key === "z";',
        '(e as KeyboardEvent).ctrlKey && (e as KeyboardEvent).key === "z";',
        '((e))!.ctrlKey && ((e as KeyboardEvent)).key === "z";',
        'e.ctrlKey && e.nativeEvent.key === "z";',
        'e.nativeEvent.ctrlKey && e.nativeEvent.key === "z";',
        'this.ev.ctrlKey && this.ev.key === "z";',
        'evs[0].ctrlKey && evs[0].key === "z";',
        'const g = { e }; g.e.ctrlKey && g.e.key === "z";',
        'e?.ctrlKey && e?.key === "z";',
      ],
    },
    {
      // Also closed by iteration 8 — found by probing for a SIXTH class
      // rather than by re-reading the five that were already written down.
      name: "modifier and key read in an expression-bodied position",
      caught: true,
      spellings: [
        'const isUndo = (ev: KeyboardEvent) => ev.ctrlKey && ev.key === "z";',
        'const isUndo2 = (ev: KeyboardEvent) => ev.ctrlKey && /^[a-z]$/.test(ev.key);',
        'switch (true) { case e.ctrlKey: if (e.key === "z") act(); }',
        'switch (true) { case e.ctrlKey && !e.shiftKey: if (e.key === "z") act(); }',
      ],
    },
    {
      // Was an ESCAPE until iteration 8: the arrow-concise-body arm above
      // reaches the dynamic `e[m]` inside the callback, which the scanner
      // already counts as both a possible key read and a possible modifier
      // assertion. The floor SHRANK; recording that is the point of pinning
      // a class rather than a string.
      name: "modifier asserted through a data table inside a nested callback",
      caught: true,
      spellings: [
        'const MODS = ["ctrlKey"]; if (MODS.every((m) => e[m])) { if (e.key === "z") act(); }',
        'const MODS = ["ctrlKey"]; if (MODS.some((m) => e[m])) { if (e.key === "z") act(); }',
        'const MODS = ["ctrlKey"]; if (MODS.filter((m) => e[m]).length > 0) { if (e.key === "z") act(); }',
      ],
    },

    /* ── the floor ── */

    {
      name: "key test in a helper whose parameter is neither event-named nor typed",
      caught: false,
      why:
        "ESCAPES, interprocedural: the modifier is in the CALLER and the key test in " +
        "the callee, and the callee's receiver carries no evidence at all — no typed " +
        "parameter, no conventional name, no unambiguous field read. Closing it needs " +
        "a call graph.",
      spellings: [
        'function h(x) { if (x.key === "z") act(); } if (e.ctrlKey) h(e);',
        'const h2 = (x) => { if (x.key === "z") act(); }; if (e.ctrlKey) h2(e);',
      ],
    },
    {
      name: "key test in a helper whose parameter IS a recognised event",
      caught: false,
      why:
        "ESCAPES, interprocedural, and STRICTLY WEAKER than the class above — the " +
        "receiver is recognised (`x: KeyboardEvent`), the key READ is counted, and only " +
        "the modifier is missing because it is asserted at the call site. This is the " +
        "refactor a reader reaches for when hoisting a chord test out of a handler, so " +
        "it is the escape most likely to be written by accident. Closing it needs the " +
        "same call graph: the guard walk would have to follow `h(e)` into `h`.",
      spellings: [
        'function h3(x: KeyboardEvent) { if (x.key === "z") act(); } if (e.ctrlKey) h3(e);',
        'const h4 = (x: KeyboardEvent) => { if (x.key === "z") act(); }; if (e.ctrlKey) h4(e);',
        'function h5(ev) { if (ev.key === "z") act(); } if (e.ctrlKey) h5(e);',
      ],
    },
    {
      name: "the key value is extracted by a helper and returned",
      caught: false,
      why:
        "ESCAPES, interprocedural: the field name is read inside the helper and only " +
        "its VALUE crosses back, so nothing at the comparison site names a key field.",
      spellings: [
        'const g = (o) => o.key; e.ctrlKey && g(e) === "z";',
        'function g2(o) { return o.key; } e.ctrlKey && g2(e) === "z";',
      ],
    },
    {
      name: "positional read with no field name anywhere",
      caught: false,
      why:
        "ESCAPES, fully dynamic: the key is addressed by POSITION in a derived " +
        "collection, so there is no field name, no receiver, and no literal for any " +
        "syntactic rule to key on.",
      spellings: [
        'e.ctrlKey && Object.values(e)[3] === "z";',
        'e.ctrlKey && Object.values(e).at(3) === "z";',
        'e.ctrlKey && Object.entries(e)[3][1] === "z";',
      ],
    },
  ];

  /** The floor, as a NUMBER, so shrinking it is as loud as growing it. */
  const ESCAPING_CLASS_COUNT = 4;

  it("catches every spelling of every class it claims to catch", () => {
    for (const c of PROBE_CLASSES.filter((x) => x.caught)) {
      for (const snippet of c.spellings) {
        expect(claimsInSnippet(snippet).length, `${c.name}: ${snippet}`).toBeGreaterThan(0);
        expect(
          claimsInRealFile(snippet).length,
          `in-file — ${c.name}: ${snippet}`,
        ).toBeGreaterThan(0);
      }
    }
  });

  it("escapes on every spelling of every class it admits escaping", () => {
    // The direction that matters most: a variant of a declared escape that
    // is quietly CAUGHT means the floor moved and this file did not say so.
    for (const c of PROBE_CLASSES.filter((x) => !x.caught)) {
      expect(c.why, `${c.name} must say why it escapes`).toBeTruthy();
      for (const snippet of c.spellings) {
        expect(claimsInSnippet(snippet), `${c.name}: ${snippet}`).toEqual([]);
        expect(claimsInRealFile(snippet), `in-file — ${c.name}: ${snippet}`).toEqual([]);
      }
    }
  });

  it("has exactly the number of escaping classes it declares", () => {
    const escaping = PROBE_CLASSES.filter((c) => !c.caught);
    expect(escaping.map((c) => c.name)).toEqual([
      "key test in a helper whose parameter is neither event-named nor typed",
      "key test in a helper whose parameter IS a recognised event",
      "the key value is extracted by a helper and returned",
      "positional read with no field name anywhere",
    ]);
    expect(escaping).toHaveLength(ESCAPING_CLASS_COUNT);
    // Every class carries more than one spelling, or it is a pinned string
    // wearing a class's name — the exact defect this rework replaced.
    for (const c of PROBE_CLASSES) {
      expect(c.spellings.length, `${c.name} needs >1 spelling`).toBeGreaterThan(1);
    }
  });

  it("finds key reads at all", () => {
    // Guards against the whole pass silently matching nothing — a green
    // scan of an empty set is the failure mode this file exists to avoid.
    const reads = SCANS.reduce((n, s) => n + s.scan.keyReads, 0);
    expect(SCANS.length).toBeGreaterThan(50);
    expect(reads).toBeGreaterThan(100);
  });
});

/* ── A. every claim in the tree is allowlisted, spelled out ──────────── */

describe("hand-rolled chord claims", () => {
  it("pins every modifier-qualified key claim in src/, wherever it lives", () => {
    const found: Record<string, string[]> = {};
    for (const { rel, scan } of SCANS) {
      if (scan.claims.length === 0) continue;
      found[rel] = scan.claims.map((c) => c.spelling).sort();
    }
    expect(found).toEqual(KNOWN_KEY_CLAIMS);
  });

  it("lets NO window/document listener hand-roll a chord the table could own", () => {
    const offenders: string[] = [];
    for (const { rel, scan } of SCANS) {
      if (!GLOBAL_LISTENER_FILES.has(rel)) continue;
      for (const claim of scan.claims) {
        if (!claim.modifiers.some((t) => CONTROL_TAGS.has(t))) continue;
        offenders.push(`${rel}:${claim.line} claims ${claim.spelling} — ${claim.text}`);
      }
    }
    // An app-wide claim outside the table is the double-fire itself, not a
    // documentation gap: two window listeners on one target both run.
    expect(offenders).toEqual([]);
  });

  it("still selects the files whose claims the old scanner could not reach", () => {
    // scrollKeys.ts registers no listener of its own. Losing it again — by
    // a selection rule, a prefilter, or a walk that stops early — would
    // restore the exact blind spot, so its presence is asserted directly
    // rather than inferred from the equality above.
    const scrollKeys = SCANS.find((s) => s.rel === "components/terminal/scrollKeys.ts");
    expect(scrollKeys, "scrollKeys.ts must be scanned").toBeDefined();
    expect(scrollKeys?.scan.claims).toHaveLength(8);
  });
});

/* ── chord claims routed through the table, extracted from source ────── */

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

/**
 * ROUTED claims — calls to the sanctioned predicates.
 *
 * Text-matched on purpose, and safely: a call to a named function has one
 * spelling, fixed by the function's name. The open-ended space that broke
 * five scanners is the HAND-ROLLED side, and that side is now the AST's
 * job. Silence here is caught by "finds the claims it is meant to police".
 */
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

// The chord module itself only MENTIONS the call shapes in its docstring;
// it is the table, not a claimant.
const CLAIMS = FILES.filter((f) => !MECHANISM_FILES.has(f.rel)).flatMap((f) =>
  claimsIn(f.rel, f.source),
);

describe("chord registries", () => {
  it("finds the claims it is meant to police", () => {
    expect(CLAIMS.length).toBeGreaterThan(20);
    expect(GLOBAL_LISTENER_FILES.size).toBeGreaterThan(10);
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

/* ── D. key-dispatch registries ──────────────────────────────────────── */

describe("key-dispatch registries", () => {
  it("has exactly the documented `switch (e.key)` registries", () => {
    const found = SCANS.filter((s) => s.scan.switchRegistry).map((s) => s.rel);
    expect(found.sort()).toEqual([...KNOWN_SWITCH_KEY_REGISTRIES].sort());
  });
});
