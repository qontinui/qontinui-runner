/**
 * keyFieldReads — MECHANISM A of the global-chord enforcement: **coverage,
 * by ban.**
 *
 * ## Why a second mechanism instead of a seventh widening
 *
 * `keyClaimScan.ts` (mechanism B) answers *which* chord a file claims. It
 * does that by recognising a key READ inside a modifier assertion, which is
 * a judgement about the SHAPE of an expression — and six rounds of widening
 * have each produced a fresh escape set, because the space of shapes is
 * open-ended. Iteration 9 measured the state after the sixth and found at
 * least six escaping classes that the test's own declared floor did not
 * name, while that file declared `ESCAPING_CLASS_COUNT = 4` and passed
 * silently. A floor that is wrong is worse than no floor: it converts an
 * unknown into a false assurance.
 *
 * This module does not try to be a better shape-recogniser. It asks a
 * strictly easier question that has a structural answer:
 *
 *   > Does this file READ a field that only a keyboard event has?
 *
 * A field READ is a syntactic fact about a NAME, not about a comparison.
 * Parenthesising the receiver, destructuring it, aliasing it, writing the
 * comparison Yoda-style, dispatching with `switch`, testing with a regex,
 * going through `Reflect.get`, or flipping the polarity all leave the field
 * name exactly where it was. So this mechanism is spelling-independent BY
 * CONSTRUCTION rather than by enumeration, and its answer to "has a new
 * file started claiming chords?" has **no false negatives** short of the
 * declared escapes at the bottom of this comment.
 *
 * The price is that it cannot say WHAT was claimed — which is precisely
 * what mechanism B is for. The two divide the work:
 *
 *   A (here)  COVERAGE — no file outside an explicit roster may read these
 *             fields at all. Answers "who touches the keyboard".
 *   B (scan)  INVENTORY — for rostered files, which chords are claimed, so
 *             collisions can be counted.
 *
 * Every escape in B therefore degrades from *"a claim is invisible"* to
 * *"the inventory may be imprecise for a file we already know about"*.
 *
 * ## Two tiers, because two of the names are ambiguous
 *
 * `keyCode`, `which`, `ctrlKey`, `metaKey`, `altKey`, `shiftKey` and
 * `getModifierState` are names nothing else in this tree uses — measured,
 * 20 files, every one of them a genuine keyboard or pointer surface. They
 * are {@link MODIFIER_FIELDS}, and they are the CHORD-RELEVANT tier: a
 * chord is a modifier plus a key, so a file that reads none of these cannot
 * hand-roll a chord.
 *
 * `key` and `code` are not: `localStorage.key(i)`, `item.key`,
 * `response.code` are everywhere — 185 further files read one. They are
 * {@link AMBIGUOUS_KEY_FIELDS}, tier 2, and their roster is large. That is
 * not a defect to be tuned away, it is the honest count: those files DO
 * read a field by that name, and the alternative — guessing which receiver
 * is an event — is the shape-recognition problem this mechanism exists to
 * avoid. What tier 2 buys is that a file which handles a bare `Escape`, or
 * holds the key half of an interprocedural chord test, is NAMED rather than
 * invisible.
 *
 * ## What counts as a read
 *
 *   R1  `x.F`                     property access
 *   R2  `x["F"]`                  element access with a literal name
 *   R3  `const { F } = x`         binding element, incl. `{ F: local }`,
 *                                 `{ ["F"]: local }`, and a destructured
 *                                 function PARAMETER
 *   R4  `"F"` anywhere            a string literal equal to a MODIFIER
 *                                 field name — covers `Reflect.get(e, "…")`,
 *                                 `const MODS = ["ctrlKey"]; e[MODS[0]]`,
 *                                 and every other dynamic spelling that
 *                                 still has to name the field once
 *   R5  `"…e.F…"` inside a       a MODIFIER field named as a FIELD
 *       larger string             REFERENCE inside a bigger literal —
 *                                 `eval("e.ctrlKey")`,
 *                                 `new Function("e", "return e.ctrlKey")`.
 *                                 R4 tests literal EQUALITY, so iteration 11
 *                                 measured these escaping while being
 *                                 neither "assembled at runtime" (the name
 *                                 is right there) nor covered by any
 *                                 declared class. Matched only in a
 *                                 field-reference POSITION (`.F`, `["F"]`)
 *                                 rather than as a bare word, because
 *                                 `which` is an ordinary English word and a
 *                                 bare-word rule would roster every file
 *                                 with prose in it.
 *   R6  `with (e) { ctrlKey }`   inside a `with` body a field is read as a
 *                                 BARE IDENTIFIER, with no receiver, no
 *                                 access node and no literal. Iteration 11
 *                                 measured mechanism A seeing nothing at
 *                                 all. Every identifier in a `with` body is
 *                                 therefore treated as a potential read.
 *
 * R1 is not applied to an AMBIGUOUS field in CALLEE position (`x.key(i)`):
 * `KeyboardEvent.key` and `.code` are strings and are never called, so
 * excluding a call site removes no keyboard read. The modifier tier keeps
 * call position, because `getModifierState` IS a call.
 *
 * A `{ key: … }` object literal or a JSX `key={…}` attribute is a WRITE of
 * a property with that name, not a read of one, and is not counted.
 *
 * ## Declared escapes (probed, and pinned by the enforcement suite)
 *
 *   1. **A field name assembled at runtime** — `e["ctrl" + "Key"]`. No
 *      literal equal to the field name exists anywhere, so R4 cannot fire,
 *      and R5 has no reference position to anchor on either. `eval("e.ctrl"
 *      + "Key")` is this class, not R5's.
 *   2. **A positional read with no field name at all** — `Object.values(e)[2]`.
 *      Same reason; this is also mechanism B's fourth declared escape.
 *   3. **A modifier asserted in another FILE** — `import { isMod } from "./m";
 *      if (isMod(e) && e.key === "z")`. The importing file reads `key` (so
 *      it is on the tier-2 roster) but reads no modifier field, so it is
 *      not on the tier-1 roster. Closing it needs a cross-file call graph.
 *
 * All three are cross-checked in `globalChords.enforcement.test.ts`, which
 * asserts the verdict of each spelling rather than a remembered count.
 *
 * {@link hasGlobalKeyListener} carries two of its own: a registration hidden
 * behind a helper (`onKey(window, "keydown", h)`), and a TARGET this pass
 * cannot resolve to a name (`const t = getTarget(); t.addEventListener(…)`).
 * Both are probed in the enforcement suite; see its docstring for the bound.
 *
 * {@link findNonDomChordClaims} — mechanism C — is the answer to a whole
 * class neither tier could see: a chord claimed with NO keyboard-event field
 * read at all. Its own two escapes are declared beside it.
 *
 * ## Not app code
 *
 * Imported only by `keyClaimScan.ts` (itself imported only by the
 * enforcement test) and by that test. It pulls in `typescript`, a
 * devDependency, so importing it from anywhere the app entry can reach
 * would drag the compiler into the shipped bundle. It lives in `src/`
 * rather than beside the test because the test walks `src/` for every
 * extension Vite bundles and would otherwise have to special-case its own
 * helpers — instead the walk skips it by name (`MECHANISM_FILES`),
 * alongside the chord table and the scanner. Nothing ELSE is skipped by
 * name: `*.test.ts` used to be, and that exclusion is what hid `.js`,
 * `.jsx`, `.mjs`, `.cjs` and `index.html` along with it.
 */

import * as ts from "typescript";

/**
 * Field names only a keyboard (or pointer) event has. A hand-rolled chord
 * claim is impossible without reading one of them.
 */
export const MODIFIER_FIELDS: readonly string[] = [
  "altKey",
  "ctrlKey",
  "getModifierState",
  "keyCode",
  "metaKey",
  "shiftKey",
  "which",
];

/**
 * Field names a keyboard event shares with half the tree (`item.key`,
 * `response.code`). Read-worthy, but not on their own evidence of a chord.
 */
export const AMBIGUOUS_KEY_FIELDS: readonly string[] = ["code", "key"];

const MODIFIER_SET: ReadonlySet<string> = new Set(MODIFIER_FIELDS);
const AMBIGUOUS_SET: ReadonlySet<string> = new Set(AMBIGUOUS_KEY_FIELDS);

/** Which field names a file reads, split by tier. Both sorted and deduped. */
export interface KeyFieldReads {
  /** Names from {@link MODIFIER_FIELDS} this file reads. */
  modifier: string[];
  /** Names from {@link AMBIGUOUS_KEY_FIELDS} this file reads. */
  ambiguous: string[];
}

/** Global objects whose `addEventListener` reaches the whole app. */
const GLOBAL_TARGETS = new Set(["window", "document", "globalThis", "self"]);

/**
 * Receiver chains that are app-wide even though their trailing name is not
 * a global. `document.body` is where a "global" listener is idiomatically
 * hung when `document` itself will not do.
 */
const GLOBAL_CHAINS = new Set([
  "document.body",
  "document.documentElement",
  "window.document",
  "window.document.body",
]);

/** `window.onkeydown = h` is a registration too, with no call at all. */
const ON_KEY_HANDLERS = new Set(["onkeydown", "onkeyup", "onkeypress"]);

/**
 * Parse once. Both mechanisms walk the same tree, so the enforcement suite
 * parses each candidate file a single time and hands the `SourceFile` to
 * each — a full re-parse per mechanism would have roughly doubled a suite
 * that already costs ~25 s in parsing alone.
 */
export function parseSource(text: string, fileName: string): ts.SourceFile {
  return ts.createSourceFile(
    fileName,
    text,
    ts.ScriptTarget.Latest,
    /* setParentNodes */ true,
    /\.tsx$/.test(fileName) ? ts.ScriptKind.TSX : ts.ScriptKind.TS,
  );
}

/** The text of a string-ish literal node, or `null`. */
function literalText(node: ts.Node | undefined): string | null {
  if (!node) return null;
  if (ts.isComputedPropertyName(node)) return literalText(node.expression);
  if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) return node.text;
  return null;
}

/**
 * A MODIFIER field named as a FIELD REFERENCE inside a larger string — R5.
 *
 * Position-anchored (`.F`, `["F"]`, `['F']`, `` [`F`] ``) rather than a bare
 * word boundary, and that is the whole design. `which` is an ordinary English
 * word: a bare-word rule would put every file with the sentence "the zone
 * which…" on the chord-relevant tier-1 roster, which is exactly the
 * false-assurance-by-noise the two-tier split exists to avoid.
 */
const FIELD_IN_STRING: ReadonlyArray<readonly [string, RegExp]> = MODIFIER_FIELDS.map(
  (f) => [f, new RegExp(`(?:\\.\\s*|\\[\\s*["'\`])${f}\\b`)] as const,
);

/** Any node whose `.text` is literal string content the author wrote. */
type StringishNode = ts.StringLiteralLike | ts.TemplateHead | ts.TemplateMiddle | ts.TemplateTail;

/**
 * Template SPANS are included, not just whole literals: the text of
 * `` `return e.${"ctrl"}Key` `` is spread across head/middle/tail, and the
 * head of `` `e.ctrlKey ${x}` `` is where the name actually lives.
 */
function isStringish(n: ts.Node): n is StringishNode {
  return (
    ts.isStringLiteralLike(n) ||
    ts.isTemplateHead(n) ||
    ts.isTemplateMiddle(n) ||
    ts.isTemplateTail(n)
  );
}

/** Every modifier field a larger string literal names in reference position. */
function modifiersNamedInString(text: string): string[] {
  const out: string[] = [];
  for (const [name, re] of FIELD_IN_STRING) if (re.test(text)) out.push(name);
  return out;
}

/** True when `n` is the thing being CALLED, not a value being read. */
function isCallee(n: ts.Node): boolean {
  const p = n.parent;
  return !!p && (ts.isCallExpression(p) || ts.isNewExpression(p)) && p.expression === n;
}

/**
 * Every keyboard-event field name read anywhere in `sf`.
 *
 * Nothing here inspects a comparison, a receiver, or a control-flow
 * position — see the module header. That is the whole point: the answer
 * cannot be changed by re-spelling the test around the read.
 */
export function findKeyFieldReads(sf: ts.SourceFile): KeyFieldReads {
  const modifier = new Set<string>();
  const ambiguous = new Set<string>();

  const note = (name: string | null, allowAmbiguous = true): void => {
    if (name === null) return;
    if (MODIFIER_SET.has(name)) modifier.add(name);
    else if (allowAmbiguous && AMBIGUOUS_SET.has(name)) ambiguous.add(name);
  };

  const walk = (n: ts.Node, inWith: boolean): void => {
    if (ts.isPropertyAccessExpression(n)) {
      // R1. An ambiguous field in callee position (`localStorage.key(i)`)
      // is not a keyboard read: `KeyboardEvent.key` is a string.
      note(n.name.text, !isCallee(n));
    } else if (ts.isElementAccessExpression(n)) {
      // R2.
      note(literalText(n.argumentExpression), !isCallee(n));
    } else if (ts.isBindingElement(n)) {
      // R3. `{ F }`, `{ F: local }`, `{ ["F"]: local }` — including a
      // destructured PARAMETER, which has no initializer and so was
      // invisible to every rule keyed on a variable declaration.
      const named = n.propertyName ?? n.name;
      note(literalText(named) ?? (ts.isIdentifier(named) ? named.text : null));
    } else if (isStringish(n)) {
      // R4, modifier tier only. A dynamic spelling still has to name the
      // field once; `"key"` as a literal is far too common to ban.
      if (MODIFIER_SET.has(n.text)) modifier.add(n.text);
      // R5. The name inside a LARGER literal, in reference position.
      for (const name of modifiersNamedInString(n.text)) modifier.add(name);
    } else if (inWith && ts.isIdentifier(n)) {
      // R6. Inside a `with` body the receiver is implicit, so the read is a
      // bare identifier with no access node to key on.
      note(n.text, !isCallee(n));
    }
    // A `with` statement's BODY is the scope where identifiers resolve off
    // the object; its head expression is ordinary code.
    if (ts.isWithStatement(n)) {
      walk(n.expression, inWith);
      walk(n.statement, true);
      return;
    }
    ts.forEachChild(n, (c) => walk(c, inWith));
  };
  walk(sf, false);

  return {
    modifier: [...modifier].sort(),
    ambiguous: [...ambiguous].sort(),
  };
}

/**
 * True when `sf` registers a key listener on a GLOBAL target — the scope in
 * which two claimants on one chord genuinely both fire.
 *
 * This replaces a regex that hard-coded `\b(window|document)\.addEventListener\(\s*"key…`,
 * and so graded none of `globalThis.addEventListener("keydown", …)`, a bare
 * `addEventListener("keydown", …)` inside a global scope, an aliased
 * `const w = window; w.addEventListener(…)`, or a single-quoted
 * `'keydown'` — four app-wide claimants the strictest property in the suite
 * simply did not look at.
 *
 * A registration CALL is a fair thing to match structurally: its shape is
 * fixed by the DOM API. What must never be matched structurally is the
 * CLAIM, whose spelling is open-ended — that is mechanism B's job.
 */
export function hasGlobalKeyListener(sf: ts.SourceFile): boolean {
  /** `const w = window;` — an alias for a global target. */
  const globalAliases = new Set<string>(GLOBAL_TARGETS);
  /** `const EVT = "keydown";` — a hoisted event-name constant. */
  const stringConsts = new Map<string, string>();

  const collect = (n: ts.Node): void => {
    if (ts.isVariableDeclaration(n) && ts.isIdentifier(n.name) && n.initializer) {
      const init = n.initializer;
      if (ts.isIdentifier(init) && globalAliases.has(init.text)) globalAliases.add(n.name.text);
      const lit = literalText(init);
      if (lit !== null) stringConsts.set(n.name.text, lit);
    }
    ts.forEachChild(n, collect);
  };
  collect(sf);

  /**
   * The event name an argument denotes, resolving a hoisted constant, a
   * parenthesis, a cast, and a CONCATENATION of any of those.
   *
   * `addEventListener("key" + "down", h)` is the cheapest possible evasion of
   * a rule that reads the first argument as a literal, and it was a silent
   * GREEN: the registration is app-wide, the event name is fully determined
   * at parse time, and nothing in the file has to be spelled unusually. The
   * fold is bounded to `+` over literals and hoisted string consts — a name
   * assembled from a runtime value is mechanism A's declared escape 1 (a
   * field name assembled at runtime), not a new class.
   */
  const eventName = (arg: ts.Expression | undefined): string | null => {
    if (!arg) return null;
    let n: ts.Node = arg;
    while (
      ts.isParenthesizedExpression(n) ||
      ts.isNonNullExpression(n) ||
      ts.isAsExpression(n) ||
      ts.isSatisfiesExpression(n) ||
      ts.isTypeAssertionExpression(n)
    ) {
      n = n.expression;
    }
    const lit = literalText(n);
    if (lit !== null) return lit;
    if (ts.isIdentifier(n)) return stringConsts.get(n.text) ?? null;
    if (ts.isBinaryExpression(n) && n.operatorToken.kind === ts.SyntaxKind.PlusToken) {
      const left = eventName(n.left);
      const right = eventName(n.right);
      return left === null || right === null ? null : left + right;
    }
    return null;
  };

  /** `window`, `w`, `document.body`, `window.document` — or null. */
  const chain = (node: ts.Node): string | null => {
    let n = node;
    // Casts unwrap here for the same reason parentheses do: they change the
    // SPELLING of a receiver and nothing about which object it is.
    // `(window as EventTarget).addEventListener("keydown", h)` was a silent
    // GREEN — the app-wide registration was right there in the source and
    // the strictest property in the suite graded it as element-scoped,
    // because an `as` is a different node kind from a parenthesis.
    while (
      ts.isParenthesizedExpression(n) ||
      ts.isNonNullExpression(n) ||
      ts.isAsExpression(n) ||
      ts.isSatisfiesExpression(n) ||
      ts.isTypeAssertionExpression(n)
    ) {
      n = n.expression;
    }
    if (ts.isIdentifier(n)) return n.text;
    if (ts.isPropertyAccessExpression(n)) {
      const base = chain(n.expression);
      return base === null ? null : `${base}.${n.name.text}`;
    }
    return null;
  };

  const isGlobalTarget = (node: ts.Node): boolean => {
    const c = chain(node);
    if (c === null) return false;
    return globalAliases.has(c) || GLOBAL_CHAINS.has(c);
  };

  let found = false;
  const walk = (n: ts.Node): void => {
    if (found) return;
    if (ts.isCallExpression(n)) {
      let global = false;
      const callee = ts.isNonNullExpression(n.expression) ? n.expression.expression : n.expression;
      if (ts.isIdentifier(callee) && callee.text === "addEventListener") {
        // A bare `addEventListener("keydown", …)` resolves to `window`.
        global = true;
      } else if (ts.isPropertyAccessExpression(callee) && callee.name.text === "addEventListener") {
        global = isGlobalTarget(callee.expression);
      }
      if (global && /^key/.test(eventName(n.arguments[0]) ?? "")) {
        found = true;
        return;
      }
    }
    // `window.onkeydown = h` — a registration with no call in it, and so
    // invisible to every rule keyed on `addEventListener`.
    if (
      ts.isBinaryExpression(n) &&
      n.operatorToken.kind === ts.SyntaxKind.EqualsToken &&
      ts.isPropertyAccessExpression(n.left) &&
      ON_KEY_HANDLERS.has(n.left.name.text.toLowerCase()) &&
      isGlobalTarget(n.left.expression)
    ) {
      found = true;
      return;
    }
    ts.forEachChild(n, walk);
  };
  walk(sf);
  return found;
}

/* ── mechanism C: a chord claimed with NO keyboard-event field read ──── */

/**
 * The words a chord SPELLING can start a modifier segment with.
 *
 * Every keybinding library that takes a chord as text agrees on this
 * vocabulary — Tauri's `register("CommandOrControl+J")`, Electron
 * accelerators, `hotkeys-js`, `react-hotkeys-hook`'s `useHotkeys("ctrl+j")`,
 * Mousetrap's `bind("mod+j")`. That is what makes {@link CHORD_STRING} a rule
 * about the CLAIM rather than about one library: a lib this repo has never
 * heard of is caught on its first use, because the chord still has to be
 * spelled.
 */
const CHORD_MODIFIER_WORDS: readonly string[] = [
  "alt",
  "cmd",
  "cmdorctrl",
  "command",
  "commandorcontrol",
  "control",
  "ctrl",
  "meta",
  "mod",
  "option",
  "shift",
  "super",
  "win",
];

/**
 * A whole string literal that IS a chord spelling — `Ctrl+J`,
 * `CommandOrControl+Shift+P`, `mod+k`.
 *
 * Anchored at both ends and requiring at least one modifier segment, so
 * `"a+b"`, `"1 + 2"` and prose containing a plus are not chords. The trailing
 * segment is the key and may be a letter, a digit or a named key (`F3`,
 * `ArrowUp`, `Escape`).
 */
const CHORD_STRING = new RegExp(
  `^(?:(?:${CHORD_MODIFIER_WORDS.join("|")})\\s*\\+\\s*)+[a-z0-9][\\w]*$`,
  "i",
);

/**
 * Keybinding APIs that take a NUMERIC constant rather than a chord string,
 * so nothing in the call names the chord in text.
 *
 * `@monaco-editor/react` is a dependency of this repo, and
 * `ed.addCommand(KeyMod.CtrlCmd | KeyCode.KeyJ, act)` is the ordinary way to
 * claim `Ctrl+J` inside the editor — reading no `KeyboardEvent` field, naming
 * no chord string, and so invisible to mechanisms A and B alike. Iteration 12
 * planted eleven live app-wide `Ctrl+J` claimants at once and the suite stayed
 * 33/33 green; this is the class most of them belonged to.
 *
 * This roster IS enumerative, unlike {@link CHORD_STRING}, and that is
 * declared rather than papered over: a keybinding API that takes a numeric
 * constant under a name not listed here escapes. Every entry is falsified by
 * `keyRules.mutation.test.ts`.
 */
const KEYBINDING_CALLS: readonly string[] = [
  "addAction",
  "addCommand",
  "addKeybinding",
  "registerKeybinding",
];

/** Monaco's keybinding constant namespaces — the operands of the call above. */
const KEYBINDING_NAMESPACES: readonly string[] = ["KeyCode", "KeyMod"];

/**
 * The browser's own no-JavaScript accelerator. `<button accessKey="j">` makes
 * the platform fire the button on Alt+J (or Ctrl+Alt+J), with no listener, no
 * field read and no library.
 */
const ACCESS_KEY = "accesskey";

/** What mechanism C found in one file. All sorted and deduped. */
export interface NonDomChordClaims {
  /** Chord spellings written as text — `ctrl+j`, lowercased. */
  chordStrings: string[];
  /** Keys claimed through the `accessKey` platform accelerator. */
  accessKeys: string[];
  /** Keybinding-constant APIs the file calls, by name. */
  keybindingApis: string[];
}

const KEYBINDING_CALL_SET: ReadonlySet<string> = new Set(KEYBINDING_CALLS);
const KEYBINDING_NAMESPACE_SET: ReadonlySet<string> = new Set(KEYBINDING_NAMESPACES);

/**
 * Chord claims that read NO keyboard-event field — mechanism C.
 *
 * Mechanisms A and B both start from a `KeyboardEvent` field read. A chord
 * claimed through a library, through Monaco's numeric constants, or through
 * `accessKey` reads none, so both are structurally blind to it: the file
 * lands on neither roster, `SCANS` never runs on it, and
 * `globalListenerOffenders` never sees it either because there is no
 * `addEventListener` to grade.
 *
 * The recognition is deliberately of two different shapes, because the class
 * is two different things:
 *
 *   C1  a chord SPELLING as text — spelling-independent across libraries, in
 *       the same way mechanism A is spelling-independent across comparisons:
 *       whatever the API, the chord has to be named.
 *   C2  `accessKey`, structurally — a JSX attribute, a property write, or
 *       `setAttribute("accesskey", …)`.
 *   C3  a keybinding-constant API by NAME — enumerative, and declared as such
 *       above.
 */
export function findNonDomChordClaims(sf: ts.SourceFile): NonDomChordClaims {
  const chordStrings = new Set<string>();
  const accessKeys = new Set<string>();
  const keybindingApis = new Set<string>();

  const walk = (n: ts.Node): void => {
    // C1 — a chord spelling written as text, wherever it appears.
    if (isStringish(n) && CHORD_STRING.test(n.text.trim())) {
      chordStrings.add(n.text.trim().toLowerCase().replace(/\s+/g, ""));
    }

    // C2 — `accessKey`, in all three spellings the platform accepts.
    if (ts.isJsxAttribute(n) && n.name.getText(sf).toLowerCase() === ACCESS_KEY) {
      accessKeys.add(literalText(n.initializer) ?? "?");
    } else if (
      ts.isPropertyAccessExpression(n) &&
      n.name.text.toLowerCase() === ACCESS_KEY &&
      ts.isBinaryExpression(n.parent) &&
      n.parent.operatorToken.kind === ts.SyntaxKind.EqualsToken &&
      n.parent.left === n
    ) {
      accessKeys.add(literalText(n.parent.right) ?? "?");
    } else if (isStringish(n) && n.text.toLowerCase() === ACCESS_KEY) {
      accessKeys.add("?");
    }

    // C3 — a keybinding-constant API, by call name or by constant namespace.
    if (ts.isCallExpression(n)) {
      const callee = n.expression;
      const name = ts.isPropertyAccessExpression(callee)
        ? callee.name.text
        : ts.isIdentifier(callee)
          ? callee.text
          : null;
      if (name !== null && KEYBINDING_CALL_SET.has(name)) keybindingApis.add(name);
    }
    if (ts.isIdentifier(n) && KEYBINDING_NAMESPACE_SET.has(n.text)) keybindingApis.add(n.text);
    if (ts.isPropertyAccessExpression(n) && KEYBINDING_NAMESPACE_SET.has(n.name.text)) {
      keybindingApis.add(n.name.text);
    }

    ts.forEachChild(n, walk);
  };
  walk(sf);

  return {
    chordStrings: [...chordStrings].sort(),
    accessKeys: [...accessKeys].sort(),
    keybindingApis: [...keybindingApis].sort(),
  };
}

/**
 * The PREFILTER, derived from the rule tables rather than hand-listed beside
 * them.
 *
 * A file whose text matches none of these cannot contain a key field read
 * (mechanisms A and B both need the field's NAME in the source), a global key
 * registration (which must name its event), or a mechanism-C claim (a chord
 * string must name a modifier word before its `+`; a keybinding API and
 * `accessKey` must be named).
 *
 * It is DERIVED because the hand-written predecessor drifted the moment a
 * rule moved. `COULD_READ_A_KEY` was `/\b(?:…|keydown|keyup|keypress)\b/`,
 * and `\bkeydown\b` does not match inside `onkeydown` — so
 * `window.onkeydown = (ev) => { if (isChord(ev, "Ctrl+J")) act(); }` was
 * never PARSED AT ALL, while a passing unit test asserted
 * `listens("window.onkeydown = h;") === true`. A rule the pipeline can never
 * feed is a fake falsification; the fix belongs here, not in deleting the
 * test. The event names are therefore matched without a leading word
 * boundary, and the whole pattern is case-insensitive so `onKeyDown` and
 * `KEYDOWN` reach the parser too.
 */
export const COULD_CLAIM_A_CHORD: RegExp = new RegExp(
  [
    // Field names — a read has to name the field.
    `\\b(?:${[...MODIFIER_FIELDS, ...AMBIGUOUS_KEY_FIELDS].join("|")})\\b`,
    // Event names — a registration has to name its event, and `onkeydown`
    // has no word boundary before `keydown`.
    "key(?:down|up|press)",
    // Mechanism C.
    `(?:${CHORD_MODIFIER_WORDS.join("|")})\\s*\\+`,
    `\\b(?:${[...KEYBINDING_CALLS, ...KEYBINDING_NAMESPACES].join("|")})\\b`,
    ACCESS_KEY,
  ].join("|"),
  "i",
);
