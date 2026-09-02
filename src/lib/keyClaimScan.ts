/**
 * keyClaimScan — a TypeScript-AST scanner that finds every KEYBOARD CHORD
 * CLAIM in a source file, whatever spelling it is written in.
 *
 * ## Why this is not another regex
 *
 * The same defect has now landed six times: a surface claims a chord with
 * its own hand-rolled key test, the chord table never hears about it, and
 * two handlers fire on one press. Each time, the enforcement scanner was
 * widened by one more regex alternative — and each widening produced a
 * fresh escape set. Iteration 7 of the manual-test loop injected 27
 * spellings into the regex scanner: 13 were caught and 14 walked straight
 * through, including
 *
 *     const { key, ctrlKey } = e; if (ctrlKey && key === "z")   ← live in src/
 *     const k = e.key;            if (e.ctrlKey && k === "z")
 *     e.ctrlKey && /^[1-8]$/.test(e.key)
 *     e.ctrlKey && e.keyCode === 90        /  e.which === 90
 *     if (isMod(e)) { if (e.key === "z") }
 *     e.ctrlKey && e["key"] === "z"
 *     e.getModifierState("Control") && e.key === "z"
 *     e.ctrlKey && KEYMAP[e.key]  /  e.key.startsWith("z")  /  e.key.match(/^z$/)
 *     e.ctrlKey && e.key.localeCompare("z") === 0
 *
 * and — the sharpest signal available that the approach itself was wrong —
 * `matchesDigitChord`'s OWN body, `/^[0-9]$/.test(e.key)`, a spelling the
 * test policing range defects could not see. The mechanism meant to catch
 * range claims was written in a range spelling it was blind to.
 *
 * A text matcher cannot enumerate an open-ended space of spellings. That is
 * not a tuning problem; it is the shape of the tool. So the recognition is
 * inverted here, in two steps that are together spelling-independent BY
 * CONSTRUCTION:
 *
 *   1. **Recognise the READ, not the comparison.** A claim is "the value of
 *      a keyboard event's key field is consulted somewhere a modifier is
 *      positively asserted". WHAT is done with the value — `===`, a Yoda
 *      `===`, `>=`/`<=`, `includes`, `has`, `startsWith`, `localeCompare`,
 *      `RegExp.test`, a `switch`, a lookup table, a hoisted `const` — is
 *      never inspected, so it cannot be spelled around. The entire
 *      comparison family collapses to one rule.
 *
 *   2. **Resolve the plumbing with the AST, not with more alternation.**
 *      Destructuring (`const { key, ctrlKey } = e`), aliasing (`const k =
 *      e.key`), bracket access (`e["key"]`), the legacy `keyCode`/`which`
 *      fields, `getModifierState(…)`, and a modifier hoisted into a helper
 *      call are all resolved structurally. Comments are not nodes, so the
 *      old "blank out the comments first" pass (these modules DOCUMENT the
 *      defective spellings they replaced) is gone with the problem.
 *
 * ## This is MECHANISM B of two, and that is the whole point
 *
 * Recognising a claim by the SHAPE of its expression is what this module
 * does, and six rounds of evidence say the shape space is open-ended: every
 * widening produced a fresh escape set, and the enforcement suite's declared
 * floor of four escaping classes was wrong while passing green — six classes
 * iteration 9 of the manual-test loop measured, and five more this rework
 * found by probing. So the coverage question was moved OUT of here, into
 * {@link ./keyFieldReads} — mechanism A, a ban on READING a keyboard-event
 * field outside an explicit roster.
 *
 * That was the direction an earlier version of this header considered and
 * REJECTED, on two grounds that turned out to be one real objection and one
 * false dilemma. The real one: a ban answers "who may read", never "what did
 * they claim", so it cannot inventory a claim set or count a collision. The
 * false one: that a blanket ban would flag every legitimate bare-key handler
 * and so "the allowlist, not the rule, would carry the meaning". A large
 * roster IS the meaning — a file that handles a bare `Escape` being NAMED
 * rather than invisible is the property, and its size is MEASURED (20 files
 * on the modifier tier and 184 on the ambiguous one when this was written)
 * rather than guessed at "dozens". Both objections dissolve once the two are
 * run side by side instead of chosen between.
 *
 * What that split buys THIS module is a bounded blast radius. Every escape
 * below degrades from *"a claim is invisible"* to *"the inventory may be
 * imprecise for a file we already know about"* — and an escape can no longer
 * hide a collision, because the file holding it cannot be off the roster.
 *
 * `ts-morph` is still rejected: same AST, one more dependency; `typescript`
 * is already a devDependency and `createSourceFile` needs no Program, no
 * tsconfig resolution and no type checker, so the scan stays a pure
 * syntactic pass over text.
 *
 * ## The escape sets THIS scanner had
 *
 * Inverting the recognition removed the regex escape set; it did not make
 * the scanner complete, and the first version of this file said its own
 * residual gaps were "all INTERPROCEDURAL or fully dynamic — closing them
 * needs a type checker or a call graph". That framing was false of the
 * largest gap it had, and saying it is part of why the gap went
 * unlooked-for.
 *
 * **Iteration 8** found nine spellings walking through, seven of them one
 * receiver test — `ts.isIdentifier(expr)` — refusing a parenthesis, a
 * non-null assertion, a cast or one hop down a chain, and two of them
 * positions `guardModifiers` had no arm for. The miss was SILENT rather than
 * an over-report because the receiver test was ASYMMETRIC:
 * `positiveModifiers` matched ANY property access, so the modifier half of a
 * claim was still seen and only the key half went missing — and a claim
 * needs both.
 *
 * **Iteration 9** found six more classes, all of them mechanism bugs rather
 * than exotic spellings:
 *
 *     if (!e.ctrlKey) return; if (e.key === "z") act();   negation counted twice
 *     !!e.ctrlKey && e.key === "z"                        `!!` not folded
 *     switch (e.key) { case "k": if (e.ctrlKey) act(); }  case ARM never read
 *     switch (true) { case e.ctrlKey && e.key === "z": }  clause expr excluded
 *     getEv().ctrlKey && getEv().key === "z"              call/await receiver
 *     addEventListener("keydown", ({key, ctrlKey}) => …)  destructured PARAM
 *     this.isMod(e) / mods.isMod(e) / isMod.call(…)       non-identifier callee
 *     const [c1] = [e.ctrlKey]  /  (() => e.ctrlKey)()    array hoist, IIFE
 *
 * **This rework's own probe** found five more that iteration 9 did not list,
 * which is the whole argument for probing rather than re-reading:
 *
 *     if (e.key === "z") { if (e.ctrlKey) act(); }        the MIRROR of a
 *                                                         pinned RED row —
 *                                                         the guard walk only
 *                                                         ever looked UP
 *     if (!e.ctrlKey) { return; } else { … }              guard disqualified
 *                                                         by an `else` that
 *                                                         changes nothing
 *     let m = false; m ||= e.ctrlKey;                     alias by ASSIGNMENT
 *     class C { hit = e.ctrlKey && e.key === "z"; }       class field, no arm
 *     export default e.ctrlKey && e.key === "z";          same, other end
 *
 * All are closed, each is pinned by a mutation row and a probe class in both
 * arms, and each fix was falsified by reverting it and confirming the suite
 * reds. What remains is declared, per class with several spellings, in
 * `globalChords.enforcement.test.ts` — with no free-standing COUNT, which is
 * what was wrong last time.
 *
 * ## Not app code
 *
 * This module is imported ONLY by `globalChords.enforcement.test.ts`. It
 * pulls in `typescript`, a devDependency, so importing it from anywhere the
 * app entry can reach would drag the compiler into the shipped bundle. It
 * lives in `src/` rather than beside the test because the test walks `src/`
 * for every extension Vite bundles and would otherwise have to special-case
 * its own helper — instead the walk skips it by name, alongside the chord
 * table itself.
 *
 * ## Its rules are falsified, one at a time
 *
 * Every table in this file — `KEY_FIELDS`, `EVENT_NAMES`, `RECEIVER_METHODS`,
 * `COMPARISON_OPS`, all of them — is enumerated entry by entry by
 * `keyRules.mutation.test.ts`, which deletes each and asserts the corpus
 * verdict moves. 61 of the 126 rule entries across this file and
 * `keyFieldReads.ts` were previously deletable with the enforcement suite
 * still 33/33 green. Add a table entry and it is enumerated automatically;
 * it must come with a row that dies without it.
 *
 * ## The one heuristic, stated plainly
 *
 * `.key` is an extremely common property name (`item.key`, `entry.key`),
 * so a bare `.key` read cannot be assumed to be a keyboard event's. A
 * receiver is treated as a keyboard event when ANY of these hold:
 * it is annotated `KeyboardEvent` / `KeyLike`; an unambiguous field
 * (`code`, `keyCode`, `which`, `ctrlKey`, `metaKey`, `altKey`, `shiftKey`,
 * `getModifierState`) is read from it anywhere in the file; it is
 * destructured with one of those; it is the parameter of an inline key
 * listener; or — the heuristic — it is NAMED like an event (`e`, `ev`,
 * `evt`, `event`, …). The heuristic widens only what counts as a key READ;
 * a claim still requires a positively asserted modifier, so its failure
 * mode is an over-report, which can only demand that a real chord be
 * routed through the table.
 */

import * as ts from "typescript";

import { parseSource } from "./keyFieldReads";

/** Fields that carry the identity of the pressed key. */
const KEY_FIELDS = new Set(["key", "code", "keyCode", "which"]);

/**
 * Key fields whose NAME is unambiguous, so a read of one identifies its
 * receiver as a keyboard event. `key` is deliberately absent.
 */
const STRONG_KEY_FIELDS = new Set(["code", "keyCode", "which"]);

/** How a chord spelling names the modifiers it requires. */
export type ModifierTag = "ctrl" | "alt" | "shift" | "mod";

/**
 * Modifier field → tag. `metaKey` folds into `ctrl` because Cmd is the
 * Ctrl alias everywhere in this app (`GlobalChord.meta`, and
 * `scrollKeys.ts`'s `cmdOrCtrl`), and the enforcement test's own
 * `spelling()` already renders a meta chord as `ctrl+…`.
 *
 * A `Map`, not an object literal, and that is not style. A plain-object
 * lookup inherits `Object.prototype`, so `MODIFIER_TAG["toLocaleString"]`
 * came back TRUTHY and every `date.toLocaleString()` in the tree was
 * recorded as a modifier assertion — which reported a plain
 * `e.key === "Enter"` accessibility handler as a chord claim with an empty
 * modifier prefix. A `Map` has no such keys.
 */
const MODIFIER_TAG = new Map<string, ModifierTag>([
  ["ctrlKey", "ctrl"],
  ["metaKey", "ctrl"],
  ["altKey", "alt"],
  ["shiftKey", "shift"],
]);

/** `getModifierState("Control")` → the tag it asserts. */
const MODIFIER_STATE_TAG = new Map<string, ModifierTag>([
  ["Control", "ctrl"],
  ["Meta", "ctrl"],
  ["OS", "ctrl"],
  ["Alt", "alt"],
  ["AltGraph", "alt"],
  ["Shift", "shift"],
]);

/**
 * Tags that make a claim expressible as a `GLOBAL_CHORDS` entry — every
 * table entry requires Ctrl.
 *
 * `shift` is NOT one of them, and the distinction is load-bearing rather
 * than cosmetic. `Shift+Enter`, `Shift+Tab`, `Shift+F3` are variant
 * selectors, not app chords, and `matchesChord` cannot express a
 * shift-only chord at all — flagging them would demand a rewrite into a
 * predicate that has no way to represent them. Shift still REFINES a
 * spelling (`shift+pageup` is a different claim from `ctrl+pageup`), so it
 * is inventoried; it just does not, alone, make a claim the table owns.
 */
export const CONTROL_TAGS: ReadonlySet<ModifierTag> = new Set<ModifierTag>(["ctrl", "alt", "mod"]);

/** Conventional identifier names for a keyboard event. See the header. */
const EVENT_NAMES = new Set([
  "e",
  "ev",
  "evt",
  "event",
  "ke",
  "keyEvent",
  "keyboardEvent",
  "nativeEvent",
  "domEvent",
]);

/**
 * The sanctioned doors into the chord table. A call to one of these is a
 * ROUTED claim, not a hand-rolled one: it reads no key field here (the
 * read happens inside `globalChords.ts`) and it asserts no modifier that
 * should taint a neighbouring bare-key test.
 */
const SANCTIONED_PREDICATES = new Set(["matchesChord", "matchesDigitChord", "isCtrlShiftChord"]);

/** Members that consult a value the way a key test does. */
const MEMBERSHIP_METHODS = new Set(["includes", "has", "indexOf", "lastIndexOf"]);

/**
 * Members called ON the key value, whose ARGUMENTS carry the key literals.
 *
 * `toLowerCase` and `toUpperCase` were entries here and were DEAD: they take
 * no arguments, so `keyLiterals`' `p.arguments.flatMap(collectStrings)` was
 * always empty, the arm never returned, and the walk fell through to the
 * enclosing comparison — which is where `e.key.toLowerCase() === "z"` gets
 * its literal from with or without them. `keyRules.mutation.test.ts` measured
 * that deleting either changed no verdict in the corpus, and no row can be
 * written that it would change, because a rule keyed on arguments cannot
 * matter to a method that has none. Deleted rather than left as a rule
 * nothing falsifies.
 */
const RECEIVER_METHODS = new Set([
  "startsWith",
  "endsWith",
  "localeCompare",
  "match",
  "matchAll",
  "search",
  "includes",
  "indexOf",
  "charCodeAt",
  "codePointAt",
  "normalize",
]);
const APPLIED_METHODS = new Set(["test", "exec"]);

/** A modifier-qualified key claim found in one file. */
export interface KeyClaim {
  /** `ctrl+shift+k`, `shift+pageup`, `ctrl+?` when no literal is legible. */
  spelling: string;
  /** The modifier tags asserted around the read. */
  modifiers: ModifierTag[];
  /** True when a tag in {@link CONTROL_TAGS} is asserted. */
  control: boolean;
  /** The expression text, for the failure message. */
  text: string;
  /** 1-based line in the scanned source. */
  line: number;
}

export interface FileScan {
  /** Every modifier-qualified key claim, deduped by spelling. */
  claims: KeyClaim[];
  /** Key reads found at all — the "did the scanner see anything" counter. */
  keyReads: number;
  /** True when the file dispatches on a key value with a `switch`. */
  switchRegistry: boolean;
}

const FUNCTION_LIKE = (n: ts.Node): boolean =>
  ts.isFunctionDeclaration(n) ||
  ts.isFunctionExpression(n) ||
  ts.isArrowFunction(n) ||
  ts.isMethodDeclaration(n) ||
  ts.isGetAccessor(n) ||
  ts.isSetAccessor(n) ||
  ts.isConstructorDeclaration(n);

/** Assignments that can carry a modifier or key value into an identifier. */
const ASSIGNMENT_OPS = new Set<ts.SyntaxKind>([
  ts.SyntaxKind.EqualsToken,
  ts.SyntaxKind.BarBarEqualsToken,
  ts.SyntaxKind.AmpersandAmpersandEqualsToken,
  ts.SyntaxKind.QuestionQuestionEqualsToken,
]);

const COMPARISON_OPS = new Set<ts.SyntaxKind>([
  ts.SyntaxKind.EqualsEqualsToken,
  ts.SyntaxKind.EqualsEqualsEqualsToken,
  ts.SyntaxKind.ExclamationEqualsToken,
  ts.SyntaxKind.ExclamationEqualsEqualsToken,
  ts.SyntaxKind.GreaterThanToken,
  ts.SyntaxKind.GreaterThanEqualsToken,
  ts.SyntaxKind.LessThanToken,
  ts.SyntaxKind.LessThanEqualsToken,
]);

/** The text of a string-ish literal node, or `null`. */
function literalText(node: ts.Node | undefined): string | null {
  if (!node) return null;
  if (ts.isComputedPropertyName(node)) return literalText(node.expression);
  if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) return node.text;
  return null;
}

/**
 * True when `node` sits under an ODD number of `!` operators that are
 * themselves INSIDE the subtree rooted at `root` (parentheses ignored).
 *
 * Two defects motivate every word of that sentence.
 *
 * **The boundary.** The previous `isNegated(node)` climbed the real AST
 * parent chain with no boundary, so it saw negations that its CALLER had
 * already consumed. `assertedBy` resolves polarity itself — it strips the
 * `!` off `if (!e.ctrlKey) return;` and calls into the modifier walk with
 * `positive = true` — and then the walk re-applied the very `!` that had
 * just been accounted for and dropped the modifier. The result was that
 * `if (!e.ctrlKey) return; if (e.key === "z") act();`, the single most
 * idiomatic guard spelling there is, scanned GREEN. The pinned
 * `!(e.ctrlKey || e.metaKey)` row was RED only by the accident of being a
 * parenthesised BINARY, whose recursion descends to the operands and so
 * hands each leaf to the walk with no `!` above it inside the subtree.
 *
 * **The parity.** Counting instead of testing folds `!!`. `!!e.ctrlKey &&
 * e.key === "z"` — a two-character mutation of the first pinned offender —
 * was GREEN because one `!` was seen and the second was never looked for.
 */
function negatedWithin(node: ts.Node, root: ts.Node): boolean {
  let cur: ts.Node = node;
  let count = 0;
  for (;;) {
    while (cur !== root && cur.parent && ts.isParenthesizedExpression(cur.parent)) cur = cur.parent;
    if (cur === root) break;
    const p = cur.parent;
    if (
      p &&
      ts.isPrefixUnaryExpression(p) &&
      p.operator === ts.SyntaxKind.ExclamationToken &&
      p.operand === cur
    ) {
      count++;
      cur = p;
      continue;
    }
    break;
  }
  return count % 2 === 1;
}

/**
 * Strip the wrappers that change a receiver's SPELLING and nothing else:
 * `(e)`, `e!`, `e as KeyboardEvent`, `<KeyboardEvent>e`, `e satisfies …`.
 *
 * These are the cheapest possible evasion of a receiver test, and the one
 * a reviewer is least likely to see: a pair of parentheses is
 * Prettier-stable and reads as noise. `(e).ctrlKey && (e).key === "z"` was
 * a silent GREEN — not because the claim was hidden, but because the
 * receiver was no longer an `Identifier` node. Unwrapping here means the
 * rule is stated once and every caller inherits it.
 */
function unwrapReceiver(node: ts.Node): ts.Node {
  let cur = node;
  for (;;) {
    if (ts.isParenthesizedExpression(cur) || ts.isNonNullExpression(cur)) {
      cur = cur.expression;
      continue;
    }
    if (ts.isAsExpression(cur) || ts.isSatisfiesExpression(cur)) {
      cur = cur.expression;
      continue;
    }
    if (ts.isTypeAssertionExpression(cur)) {
      cur = cur.expression;
      continue;
    }
    return cur;
  }
}

/**
 * True when `fn` is called on the spot — `(() => …)()`.
 *
 * An IIFE's body is not "a different press"; it is part of the very
 * expression being evaluated, and refusing to descend into it made
 * `(() => e.ctrlKey)()` a modifier-free predicate.
 */
function isImmediatelyInvoked(fn: ts.Node): boolean {
  let cur: ts.Node = fn;
  while (cur.parent && ts.isParenthesizedExpression(cur.parent)) cur = cur.parent;
  const p = cur.parent;
  return !!p && ts.isCallExpression(p) && p.expression === cur;
}

/**
 * The NAME of a predicate being called, for any callee shape.
 *
 * The old arm demanded a bare `Identifier`, so `isMod(e)` was RED while
 * `this.isMod(e)`, `mods.isMod(e)`, `(isMod)(e)` and `isMod.call(null, e)`
 * were all GREEN — a helper is the obvious place to hoist the modifier half
 * to, and putting it on an object or a class is the obvious next step.
 */
function predicateName(callee: ts.Node): string | null {
  const c = unwrapReceiver(callee);
  if (ts.isIdentifier(c)) return c.text;
  if (ts.isPropertyAccessExpression(c)) {
    const m = c.name.text;
    if (m === "call" || m === "apply" || m === "bind") return predicateName(c.expression);
    return m;
  }
  return null;
}

/**
 * The arguments a predicate actually receives — `f.call(thisArg, e)` and
 * `f.apply(thisArg, [e])` both pass `e`, one position later and one array
 * deeper than a plain `f(e)`.
 */
function predicateArguments(callee: ts.Node, call: ts.CallExpression): readonly ts.Expression[] {
  const c = unwrapReceiver(callee);
  if (ts.isPropertyAccessExpression(c)) {
    const m = c.name.text;
    if (m === "call" || m === "bind") return call.arguments.slice(1);
    if (m === "apply") {
      const arr = call.arguments[1];
      return arr && ts.isArrayLiteralExpression(arr) ? arr.elements : [];
    }
  }
  return call.arguments;
}

/**
 * A stable key for a RECEIVER EXPRESSION — `e`, `this.ev`, `e.nativeEvent`,
 * `evs[0]` — or `null` when the shape is one this pass cannot name.
 *
 * Why a key and not an identifier name: pass 1 recorded only
 * `ts.isIdentifier(receiver)` receivers, so a keyboard event reached
 * through anything else was invisible to the whole scanner. `e.nativeEvent`
 * is React's own idiom and `nativeEvent` was already in `EVENT_NAMES`, yet
 * `e.nativeEvent.key` could never reach the test — the name was listed for
 * a node shape that never arrived. Keying by normalized TEXT generalizes
 * the existing "a receiver from which a modifier or an unambiguous key
 * field is read is a keyboard event" rule to every receiver shape, without
 * a type checker: it is the same syntactic evidence, no longer restricted
 * to one node kind.
 */
function receiverKey(node: ts.Node): string | null {
  const n = unwrapReceiver(node);
  if (ts.isIdentifier(n)) return n.text;
  if (n.kind === ts.SyntaxKind.ThisKeyword) return "this";
  if (ts.isPropertyAccessExpression(n)) {
    const base = receiverKey(n.expression);
    return base === null ? null : `${base}.${n.name.text}`;
  }
  if (ts.isElementAccessExpression(n)) {
    const base = receiverKey(n.expression);
    if (base === null) return null;
    const arg = n.argumentExpression;
    const lit = literalText(arg);
    if (lit !== null) return `${base}[${JSON.stringify(lit)}]`;
    if (ts.isNumericLiteral(arg)) return `${base}[${arg.text}]`;
    if (ts.isIdentifier(arg)) return `${base}[${arg.text}]`;
    return null;
  }
  // A CALL and an AWAIT are receivers too, and were the last two node kinds
  // `positiveModifiers` could see (it matches ANY property access) while the
  // key half could not — the exact silent-miss asymmetry the header claims
  // to have removed. `getEv().ctrlKey && getEv().key === "z"` and
  // `(await p).ctrlKey && (await p).key === "z"` were GREEN.
  //
  // Arguments are deliberately not part of the key: `getEv(1)` and
  // `getEv(2)` collapse to one receiver, which can only OVER-report, and a
  // claim still needs a positively asserted modifier on the same receiver.
  if (ts.isCallExpression(n)) {
    const base = receiverKey(n.expression);
    return base === null ? null : `${base}()`;
  }
  if (ts.isAwaitExpression(n)) {
    const base = receiverKey(n.expression);
    return base === null ? null : `await ${base}`;
  }
  return null;
}

/**
 * The trailing NAME of a receiver chain — `nativeEvent` for
 * `e.nativeEvent`, `ev` for `this.ev`, `e` for `(e)`.
 *
 * This is what lets the {@link EVENT_NAMES} heuristic apply to a chain at
 * all. Without it the heuristic only ever saw bare identifiers, so
 * `e.ctrlKey && e.nativeEvent.key === "z"` — one modifier read short of
 * the fully-qualified spelling — walked straight through.
 */
function receiverName(node: ts.Node): string | null {
  const n = unwrapReceiver(node);
  if (ts.isIdentifier(n)) return n.text;
  if (ts.isPropertyAccessExpression(n)) return n.name.text;
  if (ts.isElementAccessExpression(n)) return literalText(n.argumentExpression);
  // A CALL is deliberately absent, though {@link receiverKey} names one.
  // The NAME half is the pure heuristic ("it is called `e`, so it is
  // probably an event"), and applying it to a call would make
  // `event().key` a key read on the strength of a function's name alone.
  // A call is recognised only on EVIDENCE — a modifier or unambiguous key
  // field read from the same normalized receiver text.
  return null;
}

/**
 * True when the identifier is a NAME being declared or addressed rather
 * than a value being read.
 *
 * The binding-pattern arm matters: `const { key, shiftKey, ctrlKey } = e;`
 * (`scrollKeys.ts:47`) writes the field names as identifiers, and reading
 * them as USES made the declaration itself look like a key test guarded by
 * three modifiers — a phantom `ctrl+alt+shift+?` claim on top of the eight
 * real ones.
 */
function isDeclarationName(node: ts.Identifier): boolean {
  const p = node.parent;
  if (!p) return false;
  if (ts.isPropertyAccessExpression(p) && p.name === node) return true;
  if (ts.isPropertyAssignment(p) && p.name === node) return true;
  if (ts.isBindingElement(p) && (p.propertyName === node || p.name === node)) return true;
  if (ts.isVariableDeclaration(p) && p.name === node) return true;
  if (ts.isParameter(p) && p.name === node) return true;
  if (ts.isShorthandPropertyAssignment(p) && p.name === node) return true;
  return false;
}

/**
 * Scan one source text for chord claims.
 *
 * `fileName` only selects the parser's script kind (`.tsx` enables JSX);
 * nothing is read from disk.
 */
export function scanKeyClaims(text: string, fileName = "input.tsx"): FileScan {
  return scanKeyClaimsIn(parseSource(text, fileName));
}

/**
 * The same scan over an ALREADY-PARSED file.
 *
 * The enforcement suite runs two mechanisms over the same tree — the
 * field-read ban and this inventory — and parsing ~940 candidate files
 * twice would have doubled the suite's dominant cost.
 */
export function scanKeyClaimsIn(sf: ts.SourceFile): FileScan {
  /* ── pass 1: which identifiers hold a keyboard event ───────────────── */

  const events = new Set<string>();

  /**
   * Parameters of an inline key listener that are DESTRUCTURED rather than
   * named — `addEventListener("keydown", ({ key, ctrlKey }) => …)`.
   *
   * A destructured listener parameter escaped the scanner entirely, on
   * `window` included: the identifier spelling was RED and this one GREEN,
   * because the pass-1 arm required `ts.isIdentifier(first.name)` and the
   * pass-2 binding arm required an initializer, which a PARAMETER never
   * has. Two independent rules each assumed the other covered it.
   */
  const handlerParams = new Set<ts.Node>();

  const noteHandlerParams = (fn: ts.Node): void => {
    if (!FUNCTION_LIKE(fn)) return;
    const params = (fn as ts.FunctionLikeDeclaration).parameters;
    const first = params?.[0];
    if (!first) return;
    if (ts.isIdentifier(first.name)) events.add(first.name.text);
    else handlerParams.add(first);
  };

  const pass1 = (n: ts.Node): void => {
    if ((ts.isParameter(n) || ts.isVariableDeclaration(n)) && n.type && ts.isIdentifier(n.name)) {
      if (/KeyboardEvent|KeyLike/.test(n.type.getText(sf))) events.add(n.name.text);
    }
    // The receiver is keyed by NORMALIZED TEXT, not required to be an
    // identifier: `evs[0].ctrlKey`, `this.ev.ctrlKey` and
    // `e.nativeEvent.ctrlKey` are the same evidence as `e.ctrlKey` and were
    // simply the wrong node kind to be seen.
    if (ts.isPropertyAccessExpression(n)) {
      const p = n.name.text;
      if (MODIFIER_TAG.has(p) || STRONG_KEY_FIELDS.has(p) || p === "getModifierState") {
        const key = receiverKey(n.expression);
        if (key !== null) events.add(key);
      }
    }
    if (ts.isElementAccessExpression(n)) {
      const lit = literalText(n.argumentExpression);
      if (lit && (MODIFIER_TAG.has(lit) || STRONG_KEY_FIELDS.has(lit))) {
        const key = receiverKey(n.expression);
        if (key !== null) events.add(key);
      }
    }
    if (ts.isVariableDeclaration(n) && ts.isObjectBindingPattern(n.name) && n.initializer) {
      const props = n.name.elements.map((el) => (el.propertyName ?? el.name).getText(sf));
      if (props.some((p) => MODIFIER_TAG.has(p) || STRONG_KEY_FIELDS.has(p))) {
        const key = receiverKey(n.initializer);
        if (key !== null) events.add(key);
      }
    }
    // Inline key-listener registrations: `addEventListener("keydown", fn)`,
    // xterm's `attachCustomKeyEventHandler(fn)`, and JSX `onKeyDown={fn}`.
    if (ts.isCallExpression(n)) {
      const callee = ts.isPropertyAccessExpression(n.expression) ? n.expression.name.text : "";
      if (callee === "addEventListener" && /^key/.test(literalText(n.arguments[0]) ?? "")) {
        noteHandlerParams(n.arguments[1]);
      }
      if (callee === "attachCustomKeyEventHandler") noteHandlerParams(n.arguments[0]);
    }
    if (ts.isJsxAttribute(n) && /^onKey/.test(n.name.getText(sf))) {
      const init = n.initializer;
      if (init && ts.isJsxExpression(init) && init.expression) noteHandlerParams(init.expression);
    }
    ts.forEachChild(n, pass1);
  };
  pass1(sf);

  /**
   * True when `expr` names a keyboard event.
   *
   * Both halves are receiver-shape-independent by construction. The
   * SPELLING half unwraps `( )`, `!`, and `as`/`satisfies` casts, which
   * change nothing about what is being read. The CHAIN half keys on the
   * normalized receiver text and on the chain's trailing name, so
   * `e.nativeEvent`, `this.ev` and `evs[0]` are recognised exactly as `e`
   * is — the evidence was always the same, only the node kind differed.
   *
   * The previous `ts.isIdentifier(expr) && …` was asymmetric with
   * `positiveModifiers`, which matches ANY `PropertyAccessExpression`. That
   * asymmetry is why the gap was SILENT rather than an over-report: the
   * modifier half of a claim was still seen, the key half was not, and a
   * claim needs both.
   */
  const isEventLike = (expr: ts.Node): boolean => {
    const key = receiverKey(expr);
    if (key !== null && events.has(key)) return true;
    const name = receiverName(expr);
    return name !== null && EVENT_NAMES.has(name);
  };

  /* ── pass 2: aliases for the key value and for the modifiers ───────── */

  const keyAliases = new Set<string>();
  const modAliases = new Map<string, Set<ModifierTag>>();

  /** True when `n` IS a read of a key field. */
  const isKeyRead = (n: ts.Node): boolean => {
    if (ts.isPropertyAccessExpression(n)) {
      const p = n.name.text;
      if (!KEY_FIELDS.has(p)) return false;
      return STRONG_KEY_FIELDS.has(p) || isEventLike(n.expression);
    }
    if (ts.isElementAccessExpression(n)) {
      const lit = literalText(n.argumentExpression);
      // A field addressed by an expression this pass cannot evaluate —
      // `e[F]`, `e[MODS[0]]` — is counted as a possible key read: refusing
      // to is how `const F = "key"; e[F] === "z"` would walk through a
      // scanner whose whole premise is that the spelling must not matter.
      // The `isDynamicField(n)` conjunct that used to stand here was a
      // TAUTOLOGY — it re-tested `literalText(n.argumentExpression) === null`,
      // which is exactly the `lit === null` that selected this branch — and
      // `keyRules.mutation.test.ts` measured that deleting it changed no
      // verdict, because it could not.
      if (lit === null) return isEventLike(n.expression);
      if (!KEY_FIELDS.has(lit)) return false;
      return STRONG_KEY_FIELDS.has(lit) || isEventLike(n.expression);
    }
    // `Reflect.get(e, "key")` — and `(Reflect).get(…)`, which the
    // hard-coded `ts.isIdentifier(…) && text === "Reflect"` refused for the
    // same reason every other receiver test refused a parenthesis.
    if (
      ts.isCallExpression(n) &&
      ts.isPropertyAccessExpression(n.expression) &&
      n.expression.name.text === "get" &&
      (() => {
        const r = unwrapReceiver(n.expression.expression);
        return ts.isIdentifier(r) && r.text === "Reflect";
      })()
    ) {
      const lit = literalText(n.arguments[1]);
      const target = n.arguments[0];
      if (!target) return false;
      if (lit === null) return isEventLike(target);
      return KEY_FIELDS.has(lit) && (STRONG_KEY_FIELDS.has(lit) || isEventLike(target));
    }
    if (ts.isIdentifier(n)) return keyAliases.has(n.text) && !isDeclarationName(n);
    return false;
  };

  const containsKeyRead = (n: ts.Node): boolean => {
    let hit = false;
    const walk = (x: ts.Node): void => {
      if (hit) return;
      if (isKeyRead(x)) {
        hit = true;
        return;
      }
      ts.forEachChild(x, walk);
    };
    walk(n);
    return hit;
  };

  /**
   * Modifier tags POSITIVELY asserted inside `node`, not descending into a
   * nested function (whose body belongs to a different press).
   */
  const positiveModifiers = (node: ts.Node, predicate = false): Set<ModifierTag> => {
    const out = new Set<ModifierTag>();
    const neg = (n: ts.Node): boolean => negatedWithin(n, node);
    const walk = (n: ts.Node): void => {
      // A nested function's body belongs to a different press — UNLESS it
      // is invoked right here, in which case its body is this expression:
      // `if ((() => e.ctrlKey)()) { if (e.key === "z") act(); }` was GREEN
      // for no reason other than the node kind of a term that runs
      // immediately and unconditionally.
      if (n !== node && FUNCTION_LIKE(n) && !isImmediatelyInvoked(n)) return;
      if (ts.isPropertyAccessExpression(n)) {
        const tag = MODIFIER_TAG.get(n.name.text);
        if (tag && !neg(n)) out.add(tag);
      } else if (ts.isElementAccessExpression(n)) {
        const lit = literalText(n.argumentExpression);
        const tag = lit ? MODIFIER_TAG.get(lit) : undefined;
        if (tag && !neg(n)) out.add(tag);
        if (predicate && lit === null && isEventLike(n.expression) && !neg(n)) out.add("mod");
      } else if (ts.isIdentifier(n)) {
        const tags = modAliases.get(n.text);
        if (tags && !neg(n) && !isDeclarationName(n)) for (const t of tags) out.add(t);
      } else if (ts.isCallExpression(n)) {
        const callee = unwrapReceiver(n.expression);
        if (ts.isPropertyAccessExpression(callee) && callee.name.text === "getModifierState") {
          if (!neg(n)) {
            const lit = literalText(n.arguments[0]);
            out.add((lit ? MODIFIER_STATE_TAG.get(lit) : undefined) ?? "mod");
          }
        } else if (
          predicate &&
          predicateName(callee) !== null &&
          !SANCTIONED_PREDICATES.has(predicateName(callee) as string) &&
          predicateArguments(callee, n).some((a) => isEventLike(a)) &&
          !neg(n)
        ) {
          // `isMod(e)` — a predicate over the event whose body this pass
          // cannot see. Counted as an unresolved modifier assertion: a
          // helper is the obvious place to hoist the modifier half to.
          //
          // Only in a PREDICATE position (an `if`/`while`/ternary
          // condition). Applying it to any expression made
          // `const details = renderEventDetails(event);` an assertion, and
          // `details` a modifier alias — which then reported a plain
          // `e.key === "Enter"` accessibility handler as a chord claim.
          // Over-reporting is the safe direction, but not without limit:
          // a false claim demands a rewrite into a predicate that cannot
          // express a bare key.
          out.add("mod");
        }
      }
      ts.forEachChild(n, walk);
    };
    walk(node);
    return out;
  };

  /**
   * Modifier tags asserted by `expr` being TRUE / FALSE, tracking boolean
   * polarity through `!`, `&&` and `||`.
   *
   * Needed for the EARLY-RETURN GUARD, which the general subtree walk above
   * structurally cannot see: after
   *
   *     if (!(e.ctrlKey || e.metaKey)) return null;
   *     if (!/^[0-9]$/.test(e.key)) return null;
   *
   * the key test in the SECOND statement is a ctrl claim, but no ancestor of
   * it mentions a modifier — the assertion lives in a PRECEDING SIBLING.
   * That is not a hypothetical spelling: it is the body of `matchesDigitChord`
   * itself, and it is the most idiomatic way anyone writes a chord handler.
   *
   * Polarity is tracked rather than assumed because the direction decides
   * correctness in both directions. `if (!e.ctrlKey) return;` establishes
   * that Ctrl IS held for everything after it; `if (e.ctrlKey) return;`
   * establishes the opposite, and flagging the bare-key test that follows it
   * would be exactly the false positive the `e.key === "?" && !e.ctrlKey`
   * fixture exists to forbid.
   */
  const assertedBy = (expr: ts.Node, positive: boolean, out: Set<ModifierTag>): void => {
    let n: ts.Node = expr;
    while (ts.isParenthesizedExpression(n)) n = n.expression;
    if (ts.isPrefixUnaryExpression(n) && n.operator === ts.SyntaxKind.ExclamationToken) {
      assertedBy(n.operand, !positive, out);
      return;
    }
    if (ts.isBinaryExpression(n)) {
      const op = n.operatorToken.kind;
      const and = op === ts.SyntaxKind.AmpersandAmpersandToken;
      const or = op === ts.SyntaxKind.BarBarToken;
      // `a && b` true ⇒ both true. `!(a || b)` ⇒ both false. The mixed
      // cases (`!(a && b)`, `a || b` true) imply nothing about either side
      // on their own; `||` is nonetheless unioned when POSITIVE because
      // `e.ctrlKey || e.metaKey` is the Cmd-alias idiom and both tags fold
      // to `ctrl` anyway.
      if ((and && positive) || (or && !positive) || (or && positive)) {
        assertedBy(n.left, positive, out);
        assertedBy(n.right, positive, out);
      }
      return;
    }
    if (!positive) return;
    for (const tag of positiveModifiers(n, true)) out.add(tag);
  };

  /** True when `stmt` exits the enclosing block unconditionally. */
  const isExit = (stmt: ts.Statement | undefined): boolean => {
    if (!stmt) return false;
    if (
      ts.isReturnStatement(stmt) ||
      ts.isContinueStatement(stmt) ||
      ts.isBreakStatement(stmt) ||
      ts.isThrowStatement(stmt)
    ) {
      return true;
    }
    return ts.isBlock(stmt) && isExit(stmt.statements[stmt.statements.length - 1]);
  };

  /**
   * Modifier tags established for `stmt` by the early-return guards that
   * precede it in the same block.
   */
  const inheritedGuards = (stmt: ts.Statement): Set<ModifierTag> => {
    const out = new Set<ModifierTag>();
    const parent = stmt.parent;
    const list: readonly ts.Statement[] | undefined = parent
      ? ts.isBlock(parent) || ts.isSourceFile(parent)
        ? parent.statements
        : ts.isCaseClause(parent) || ts.isDefaultClause(parent)
          ? parent.statements
          : undefined
      : undefined;
    if (!list) return out;
    for (const prev of list) {
      if (prev === stmt) break;
      if (!ts.isIfStatement(prev)) continue;
      // An `else` arm used to disqualify the guard entirely. It should not:
      // when the THEN arm exits, the only way to reach the statements after
      // the `if` is through the false branch, so the assertion holds
      // exactly as it does without an `else`. `if (!e.ctrlKey) { return; }
      // else { … }` was therefore a silent GREEN.
      if (!isExit(prev.thenStatement)) continue;
      assertedBy(prev.expression, false, out);
    }
    return out;
  };

  /**
   * True when an object binding pattern destructures a KEYBOARD EVENT.
   *
   * Three independent kinds of evidence, because a PARAMETER has no
   * initializer to point at:
   *   - it destructures an expression that is event-like (`= e`);
   *   - it is annotated `KeyboardEvent` / `KeyLike`;
   *   - it is the parameter of an inline key listener; or
   *   - it binds a field whose NAME only a keyboard event has. That last
   *     one is the general rule and needs no plumbing at all: nothing but
   *     an event has a `ctrlKey`.
   */
  const isEventDestructure = (
    n: ts.VariableDeclaration | ts.ParameterDeclaration,
    pattern: ts.ObjectBindingPattern,
  ): boolean => {
    const init = ts.isVariableDeclaration(n) ? n.initializer : undefined;
    if (init && isEventLike(init)) return true;
    if (n.type && /KeyboardEvent|KeyLike/.test(n.type.getText(sf))) return true;
    if (ts.isParameter(n) && handlerParams.has(n)) return true;
    return pattern.elements.some((el) => {
      const prop = literalText(el.propertyName) ?? (el.propertyName ?? el.name).getText(sf);
      return MODIFIER_TAG.has(prop) || STRONG_KEY_FIELDS.has(prop);
    });
  };

  const pass2 = (n: ts.Node): void => {
    if (ts.isVariableDeclaration(n) || ts.isParameter(n)) {
      const init = ts.isVariableDeclaration(n) ? n.initializer : undefined;
      if (ts.isObjectBindingPattern(n.name) && isEventDestructure(n, n.name)) {
        for (const el of n.name.elements) {
          const prop = literalText(el.propertyName) ?? (el.propertyName ?? el.name).getText(sf);
          const local = el.name.getText(sf);
          if (KEY_FIELDS.has(prop)) keyAliases.add(local);
          const tag = MODIFIER_TAG.get(prop);
          if (tag) modAliases.set(local, new Set([tag]));
        }
      }
      // `const [c1] = [e.ctrlKey];` — the array spelling of the alias hoist
      // whose object spelling was already covered. Positional, so it needs
      // no name resolution.
      if (ts.isArrayBindingPattern(n.name) && init && ts.isArrayLiteralExpression(init)) {
        n.name.elements.forEach((el, i) => {
          if (ts.isOmittedExpression(el) || !ts.isIdentifier(el.name)) return;
          const src = init.elements[i];
          if (!src) return;
          if (isKeyRead(src)) keyAliases.add(el.name.text);
          else if (!containsKeyRead(src)) {
            const mods = positiveModifiers(src);
            if (mods.size > 0) modAliases.set(el.name.text, mods);
          }
        });
      }
      if (ts.isIdentifier(n.name) && init) {
        if (isKeyRead(init)) keyAliases.add(n.name.text);
        else if (!containsKeyRead(init)) {
          const mods = positiveModifiers(init);
          if (mods.size > 0) modAliases.set(n.name.text, mods);
        }
      }
    }
    // `m = e.ctrlKey`, `m ||= e.ctrlKey`, `m &&= …`, `m ??= …`. The alias
    // pass only ever looked at DECLARATIONS, so hoisting the modifier into
    // a pre-declared `let` — the idiom for building a flag across several
    // lines — dropped it. `let m = false; m ||= e.ctrlKey;` was GREEN.
    if (
      ts.isBinaryExpression(n) &&
      ASSIGNMENT_OPS.has(n.operatorToken.kind) &&
      ts.isIdentifier(n.left)
    ) {
      const local = n.left.text;
      if (isKeyRead(n.right)) keyAliases.add(local);
      else if (!containsKeyRead(n.right)) {
        const mods = positiveModifiers(n.right);
        if (mods.size > 0) {
          const existing = modAliases.get(local) ?? new Set<ModifierTag>();
          for (const t of mods) existing.add(t);
          modAliases.set(local, existing);
        }
      }
    }
    ts.forEachChild(n, pass2);
  };
  pass2(sf);

  /* ── pass 3: the claims ────────────────────────────────────────────── */

  /**
   * Modifier tags asserted by a nested `if`/`while` guard INSIDE a branch
   * that a key test already selected.
   *
   * `if (e.ctrlKey) { if (e.key === "z") act(); }` was RED and its mirror
   * `if (e.key === "z") { if (e.ctrlKey) act(); }` was GREEN — the ancestor
   * walk only ever looks UP, so a modifier that refines a key test from
   * BELOW was invisible. Nothing about the two orderings differs
   * semantically, and the second is what a `switch`-shaped handler
   * degenerates to when it is rewritten as an `if` chain. (The `switch`
   * form of the same defect is the case-arm rule in pass 3.)
   *
   * Only nested CONDITIONS count, not every modifier read in the branch. A
   * modifier consumed as a VALUE (`return e.shiftKey ? "prev" : "next"`)
   * refines an outcome rather than gating one. Counting it was implemented
   * and MEASURED rather than assumed: it adds `shift+enter` and `shift+f3`
   * to `TerminalFindBar.tsx` and `shift+f3` to `TerminalInstance.tsx` —
   * three shift-only spellings on two live files. Those are arguably real
   * claims, so this is an inventory DECISION, not a defect: widening the
   * pinned claim set of live files is a change to make deliberately and on
   * its own, not as a side effect of a mechanism rework. The residual is
   * declared as an escaping class in `globalChords.enforcement.test.ts`.
   */
  const branchGuards = (body: ts.Node | undefined): Set<ModifierTag> => {
    const out = new Set<ModifierTag>();
    if (!body) return out;
    const walk = (x: ts.Node): void => {
      if (FUNCTION_LIKE(x)) return;
      if (ts.isIfStatement(x) || ts.isWhileStatement(x) || ts.isDoStatement(x)) {
        for (const t of positiveModifiers(x.expression, true)) out.add(t);
      }
      ts.forEachChild(x, walk);
    };
    walk(body);
    return out;
  };

  /**
   * Modifier tags governing `n`: every enclosing CONDITION, the enclosing
   * statement's own expression, and every early-return guard preceding it
   * in an enclosing block — all the way to the top of the file.
   *
   * Walking through a `Block` into its `if` is what makes
   * `if (e.ctrlKey) { switch (e.key) { … } }` visible, which a
   * statement-window scanner structurally could not see (the guarded
   * block's `{` IS a statement boundary).
   *
   * The walk deliberately does NOT stop at a function boundary, so a key
   * read inside an inline callback — `e.ctrlKey && KEYS.some((k) => k ===
   * e.key)` — still sees the modifier its own statement asserts. That is
   * safe only because {@link positiveModifiers} refuses to descend INTO a
   * nested function: an outer `const handler = (e) => { … }` therefore
   * contributes nothing from the handler's body, so an unrelated
   * `e.ctrlKey` deep inside a handler can never taint a bare-key test
   * elsewhere in it. The two rules are a pair; changing either alone
   * reintroduces one of the two failure modes.
   */
  const guardModifiers = (n: ts.Node): Set<ModifierTag> => {
    const out = new Set<ModifierTag>();
    const add = (s: Set<ModifierTag>): void => {
      for (const t of s) out.add(t);
    };
    let cur: ts.Node = n;
    while (cur.parent) {
      const p: ts.Node = cur.parent;
      if (ts.isIfStatement(p) || ts.isWhileStatement(p) || ts.isDoStatement(p)) {
        add(positiveModifiers(p.expression, true));
        // The key read is IN the condition, so the branch it selects is
        // governed by this key — see `branchGuards`.
        if (cur === p.expression) {
          add(branchGuards(ts.isIfStatement(p) ? p.thenStatement : p.statement));
        }
      } else if (ts.isConditionalExpression(p)) {
        add(positiveModifiers(p.condition, true));
      } else if (ts.isForStatement(p) && p.condition) {
        add(positiveModifiers(p.condition, true));
      } else if (ts.isSwitchStatement(p)) {
        add(positiveModifiers(p.expression));
      } else if (ts.isExpressionStatement(p)) {
        add(positiveModifiers(p.expression));
      } else if (ts.isVariableStatement(p) || ts.isPropertyDeclaration(p)) {
        // A CLASS FIELD initializer is a variable statement wearing a
        // different node kind — `class C { hit = e.ctrlKey && e.key === "z"; }`
        // had no arm at all and scanned GREEN.
        add(positiveModifiers(p));
      } else if (ts.isExportAssignment(p)) {
        // `export default e.ctrlKey && e.key === "z";` — same hole, at the
        // other end of the file.
        add(positiveModifiers(p.expression));
      } else if (ts.isReturnStatement(p) && p.expression) {
        add(positiveModifiers(p.expression));
      } else if (ts.isArrowFunction(p) && p.body === cur && !ts.isBlock(p.body)) {
        // An arrow's CONCISE body is the same expression a `return` arm
        // would carry, and it was the only statement-shaped position with
        // no arm here. `const isUndo = (e: KeyboardEvent) => e.ctrlKey &&
        // e.key === "z";` therefore scanned GREEN — a claim whose modifier
        // sits three tokens from its key read, in one expression, and the
        // single most natural way to hoist a chord test into a predicate.
        // The `{ return … }` and ternary spellings of the same helper were
        // both RED, which is the shape of an accident rather than a limit.
        add(positiveModifiers(p.body, true));
      } else if (ts.isCaseClause(p)) {
        // `switch (true) { case e.ctrlKey: … }` — the discriminant is the
        // literal `true` and the assertion lives in the CLAUSE. The switch
        // arm above reads only the discriminant, so the clause's own test
        // was never consulted.
        //
        // The `p.expression !== cur` exclusion this arm used to carry
        // excluded the spelling with BOTH halves in the clause —
        // `switch (true) { case e.ctrlKey && e.key === "z": }` was GREEN
        // while the two-statement spelling of the same thing was RED. It
        // was meant to skip the ordinary `switch (e.key) { case "a": }`
        // registry, but in that registry the key read is in the
        // DISCRIMINANT, so no case clause is ever an ancestor of it and
        // there was nothing to exclude.
        add(positiveModifiers(p.expression, true));
      }
      // A preceding `if (!e.ctrlKey) return;` in the same block asserts the
      // modifier for everything after it — see `inheritedGuards`.
      if (ts.isStatement(cur)) add(inheritedGuards(cur));
      cur = p;
    }
    return out;
  };

  const collectStrings = (n: ts.Node | undefined): string[] => {
    if (!n) return [];
    const out: string[] = [];
    const walk = (x: ts.Node): void => {
      const lit = literalText(x);
      if (lit !== null) out.push(lit);
      if (ts.isRegularExpressionLiteral(x)) out.push(x.text);
      if (ts.isNumericLiteral(x)) out.push(x.text);
      ts.forEachChild(x, walk);
    };
    walk(n);
    return out;
  };

  /** The key literals this read is compared, cased or looked up against. */
  const keyLiterals = (n: ts.Node): string[] => {
    let cur: ts.Node = n;
    for (let hops = 0; hops < 8 && cur.parent; hops++) {
      const p: ts.Node = cur.parent;
      if (ts.isBinaryExpression(p) && COMPARISON_OPS.has(p.operatorToken.kind)) {
        const other = p.left === cur ? p.right : p.left;
        const lits = collectStrings(other);
        if (lits.length) return lits;
      }
      if (ts.isCallExpression(p)) {
        const callee = p.expression;
        if (ts.isPropertyAccessExpression(callee)) {
          const method = callee.name.text;
          if (MEMBERSHIP_METHODS.has(method) && p.arguments.some((a) => containsKeyRead(a))) {
            const lits = collectStrings(callee.expression);
            if (lits.length) return lits;
          }
          if (APPLIED_METHODS.has(method) && p.arguments.some((a) => containsKeyRead(a))) {
            const lits = collectStrings(callee.expression);
            if (lits.length) return lits;
          }
          if (RECEIVER_METHODS.has(method) && containsKeyRead(callee.expression)) {
            const lits = p.arguments.flatMap((a) => collectStrings(a));
            if (lits.length) return lits;
          }
        }
      }
      if (ts.isSwitchStatement(p) && p.expression === cur) {
        return p.caseBlock.clauses.flatMap((c) =>
          ts.isCaseClause(c) ? collectStrings(c.expression) : [],
        );
      }
      if (ts.isStatement(p) && !ts.isExpressionStatement(p) && !ts.isVariableStatement(p)) break;
      cur = p;
    }
    return [];
  };

  const claims = new Map<string, KeyClaim>();
  let keyReads = 0;
  let switchRegistry = false;

  /** Record one claim per key literal, deduped by spelling. */
  const record = (at: ts.Node, mods: Set<ModifierTag>, lits: string[], show = at.parent): void => {
    if (mods.size === 0) return;
    const order: ModifierTag[] = ["ctrl", "mod", "alt", "shift"];
    const modifiers = order.filter((t) => mods.has(t));
    const prefix = modifiers.join("+");
    const keys = lits.length > 0 ? lits : ["?"];
    const { line } = sf.getLineAndCharacterOfPosition(at.getStart(sf));
    for (const k of keys) {
      const spelling = `${prefix}+${k.toLowerCase()}`;
      if (claims.has(spelling)) continue;
      claims.set(spelling, {
        spelling,
        modifiers,
        control: modifiers.some((t) => CONTROL_TAGS.has(t)),
        text: (show ?? at).getText(sf).replace(/\s+/g, " ").slice(0, 120),
        line: line + 1,
      });
    }
  };

  const pass3 = (n: ts.Node): void => {
    if (
      ts.isSwitchStatement(n) &&
      containsKeyRead(n.expression) &&
      n.caseBlock.clauses.some((c) => ts.isCaseClause(c) && collectStrings(c.expression).length > 0)
    ) {
      switchRegistry = true;
    }
    // A `case` ARM BODY that asserts a modifier:
    //
    //     switch (e.key) { case "k": if (e.ctrlKey) act(); break; }
    //
    // scanned GREEN, while the suite's own comment asserted that "a `case`
    // arm that started testing `event.ctrlKey` WOULD be a claim, and
    // property A catches that directly". It did not: `guardModifiers` reads
    // only the switch DISCRIMINANT, and the four allowlisted switch
    // registries are exactly the files most likely to grow such an arm.
    //
    // The claim is emitted PER CLAUSE, not by unioning the arms into the
    // discriminant read — an arm that tests Ctrl must not make every OTHER
    // arm's bare key a Ctrl chord.
    if (ts.isCaseClause(n) && ts.isCaseBlock(n.parent) && ts.isSwitchStatement(n.parent.parent)) {
      const sw = n.parent.parent;
      if (containsKeyRead(sw.expression)) {
        const lits = collectStrings(n.expression);
        if (lits.length > 0) {
          const mods = guardModifiers(sw.expression);
          // `predicate = false`: an arm body is arbitrary statements, not a
          // condition, so `doThing(e);` must not read as a modifier test.
          for (const st of n.statements) for (const t of positiveModifiers(st)) mods.add(t);
          record(n, mods, lits, n);
        }
      }
    }
    if (isKeyRead(n)) {
      keyReads++;
      const mods = guardModifiers(n);
      if (mods.size > 0) record(n, mods, keyLiterals(n));
    }
    ts.forEachChild(n, pass3);
  };
  pass3(sf);

  return {
    claims: [...claims.values()].sort((a, b) => a.spelling.localeCompare(b.spelling)),
    keyReads,
    switchRegistry,
  };
}
