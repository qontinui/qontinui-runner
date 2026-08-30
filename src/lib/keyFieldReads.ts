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
 *      literal equal to the field name exists anywhere, so R4 cannot fire.
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
 * {@link hasGlobalKeyListener} carries a fourth, of its own: a registration
 * hidden behind a helper (`onKey(window, "keydown", h)`). See its docstring.
 *
 * ## Not app code
 *
 * Imported only by `keyClaimScan.ts` (itself imported only by the
 * enforcement test) and by that test. It pulls in `typescript`, a
 * devDependency, so importing it from anywhere the app entry can reach
 * would drag the compiler into the shipped bundle. It lives in `src/`
 * rather than beside the test because the test walks `src/` for `.tsx?`
 * files and would otherwise have to special-case its own helpers — instead
 * the walk skips it by name, alongside the chord table and the scanner.
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

  const walk = (n: ts.Node): void => {
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
    } else if (ts.isStringLiteral(n) || ts.isNoSubstitutionTemplateLiteral(n)) {
      // R4, modifier tier only. A dynamic spelling still has to name the
      // field once; `"key"` as a literal is far too common to ban.
      if (MODIFIER_SET.has(n.text)) modifier.add(n.text);
    }
    ts.forEachChild(n, walk);
  };
  walk(sf);

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

  /** The event name an argument denotes, resolving a hoisted constant. */
  const eventName = (arg: ts.Expression | undefined): string | null => {
    if (!arg) return null;
    const lit = literalText(arg);
    if (lit !== null) return lit;
    if (ts.isIdentifier(arg)) return stringConsts.get(arg.text) ?? null;
    return null;
  };

  /** `window`, `w`, `document.body`, `window.document` — or null. */
  const chain = (node: ts.Node): string | null => {
    let n = node;
    while (ts.isParenthesizedExpression(n) || ts.isNonNullExpression(n)) n = n.expression;
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
