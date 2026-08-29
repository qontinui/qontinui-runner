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
 * ## What was rejected, and why
 *
 * - **A seventh regex widening.** Six rounds of evidence say the next one
 *   produces the next escape set.
 * - **Banning `KeyboardEvent` field access outside an allowlist, as a lint
 *   rule.** This was the preferred direction and it is the right instinct —
 *   a ban on READING the fields is spelling-independent the same way this
 *   scanner is. It was rejected in that FORM for two reasons. A blanket ban
 *   would flag every legitimate bare-key handler in the tree (Escape,
 *   arrows, Enter — dozens of files), so the allowlist, not the rule, would
 *   carry the meaning; and it answers only "who may read", never "what did
 *   they claim", so it cannot inventory a claim set or count a collision.
 *   The INVERSION it is really asking for is kept and is the whole point of
 *   `globalChords.enforcement.test.ts`'s property A: you may not claim a
 *   modifier-qualified key ANYWHERE in `src/` unless you route it through
 *   `globalChords.ts` or are named, with your exact chord spelling, in that
 *   file's allowlist. Selection is by DATA — a key read in a modifier
 *   context — not by whether the file happens to contain an
 *   `addEventListener` call, which is precisely the selection bug that let
 *   `components/terminal/scrollKeys.ts` claim eight chords invisibly.
 * - **`ts-morph`.** Same AST, one more dependency; `typescript` is already
 *   a devDependency and `createSourceFile` needs no Program, no tsconfig
 *   resolution and no type checker, so the scan stays a pure syntactic
 *   pass over text.
 *
 * ## The escape set THIS scanner had, and why the framing was wrong
 *
 * Inverting the recognition removed the regex escape set; it did not make
 * the scanner complete, and the first version of this file said its own
 * residual gaps were "all INTERPROCEDURAL or fully dynamic — closing them
 * needs a type checker or a call graph". That framing was false of the
 * largest gap it actually had, and saying it is part of why the gap went
 * unlooked-for. Iteration 8 of the manual-test loop found nine more
 * spellings walking straight through:
 *
 *     (e).ctrlKey && (e).key === "z"
 *     e!.ctrlKey && e!.key === "z"
 *     (e as KeyboardEvent).ctrlKey && (e as KeyboardEvent).key === "z"
 *     e.ctrlKey && e.nativeEvent.key === "z"
 *     e.nativeEvent.ctrlKey && e.nativeEvent.key === "z"
 *     this.ev.ctrlKey && this.ev.key === "z"
 *     evs[0].ctrlKey && evs[0].key === "z"
 *     const isUndo = (ev: KeyboardEvent) => ev.ctrlKey && ev.key === "z"
 *     switch (true) { case e.ctrlKey: if (e.key === "z") act(); }
 *
 * Every one is PURELY SYNTACTIC and lives in a single expression. Seven of
 * them are one receiver test — `ts.isIdentifier(expr)` — refusing a
 * receiver that is a parenthesis, a non-null assertion, a cast, or one hop
 * down a chain; `nativeEvent` was already listed in {@link EVENT_NAMES}, so
 * React's own idiom had been INTENDED to be covered and could never arrive
 * in the node shape the test demanded. The other two are positions
 * `guardModifiers` had no arm for: an arrow's concise body, and a
 * `switch (true)` case clause.
 *
 * The miss was SILENT rather than an over-report because the receiver test
 * was ASYMMETRIC: `positiveModifiers` matched ANY property access, so the
 * modifier half of the claim was still seen and only the key half went
 * missing — and a claim needs both. A pair of parentheses is
 * Prettier-stable and reads to a reviewer as noise.
 *
 * All nine are closed. The residual escapes that remain really are
 * interprocedural or nameless, and `globalChords.enforcement.test.ts`
 * property E now pins them as CLASSES with several spellings each, plus a
 * count — so the floor moving in EITHER direction goes red.
 *
 * ## Not app code
 *
 * This module is imported ONLY by `globalChords.enforcement.test.ts`. It
 * pulls in `typescript`, a devDependency, so importing it from anywhere the
 * app entry can reach would drag the compiler into the shipped bundle. It
 * lives in `src/` rather than beside the test because the test walks `src/`
 * for `.tsx?` files and would otherwise have to special-case its own helper
 * — instead the walk skips it by name, alongside the chord table itself.
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
  "toLowerCase",
  "toUpperCase",
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
 * True when `node` addresses a field of `receiver` by an expression this
 * pass cannot evaluate — `e[F]`, `e[MODS[0]]`.
 *
 * Dynamic field access on a keyboard event is counted as BOTH a possible
 * key read and a possible modifier assertion. It is rare enough that the
 * over-report costs nothing, and refusing to count it is how
 * `const F = "key"; e[F] === "z"` would walk through a scanner whose whole
 * premise is that the spelling of the access must not matter.
 */
function isDynamicField(node: ts.ElementAccessExpression): boolean {
  return literalText(node.argumentExpression) === null;
}

/** True when `node` sits directly under a `!` (through parentheses). */
function isNegated(node: ts.Node): boolean {
  let cur: ts.Node = node;
  while (cur.parent && ts.isParenthesizedExpression(cur.parent)) cur = cur.parent;
  const p = cur.parent;
  return (
    !!p &&
    ts.isPrefixUnaryExpression(p) &&
    p.operator === ts.SyntaxKind.ExclamationToken &&
    p.operand === cur
  );
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
  const sf = ts.createSourceFile(
    fileName,
    text,
    ts.ScriptTarget.Latest,
    /* setParentNodes */ true,
    /\.tsx$/.test(fileName) ? ts.ScriptKind.TSX : ts.ScriptKind.TS,
  );

  /* ── pass 1: which identifiers hold a keyboard event ───────────────── */

  const events = new Set<string>();

  const noteHandlerParams = (fn: ts.Node): void => {
    if (!FUNCTION_LIKE(fn)) return;
    const params = (fn as ts.FunctionLikeDeclaration).parameters;
    const first = params?.[0];
    if (first && ts.isIdentifier(first.name)) events.add(first.name.text);
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
      if (lit === null) return isEventLike(n.expression) && isDynamicField(n);
      if (!KEY_FIELDS.has(lit)) return false;
      return STRONG_KEY_FIELDS.has(lit) || isEventLike(n.expression);
    }
    // `Reflect.get(e, "key")`.
    if (
      ts.isCallExpression(n) &&
      ts.isPropertyAccessExpression(n.expression) &&
      n.expression.name.text === "get" &&
      ts.isIdentifier(n.expression.expression) &&
      n.expression.expression.text === "Reflect"
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
    const walk = (n: ts.Node): void => {
      if (n !== node && FUNCTION_LIKE(n)) return;
      if (ts.isPropertyAccessExpression(n)) {
        const tag = MODIFIER_TAG.get(n.name.text);
        if (tag && !isNegated(n)) out.add(tag);
      } else if (ts.isElementAccessExpression(n)) {
        const lit = literalText(n.argumentExpression);
        const tag = lit ? MODIFIER_TAG.get(lit) : undefined;
        if (tag && !isNegated(n)) out.add(tag);
        if (predicate && lit === null && isEventLike(n.expression) && !isNegated(n)) out.add("mod");
      } else if (ts.isIdentifier(n)) {
        const tags = modAliases.get(n.text);
        if (tags && !isNegated(n) && !isDeclarationName(n)) for (const t of tags) out.add(t);
      } else if (ts.isCallExpression(n)) {
        const callee = n.expression;
        if (ts.isPropertyAccessExpression(callee) && callee.name.text === "getModifierState") {
          const lit = literalText(n.arguments[0]);
          out.add((lit ? MODIFIER_STATE_TAG.get(lit) : undefined) ?? "mod");
        } else if (
          predicate &&
          ts.isIdentifier(callee) &&
          !SANCTIONED_PREDICATES.has(callee.text) &&
          n.arguments.some((a) => isEventLike(a)) &&
          !isNegated(n)
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
      if (!ts.isIfStatement(prev) || prev.elseStatement) continue;
      if (!isExit(prev.thenStatement)) continue;
      assertedBy(prev.expression, false, out);
    }
    return out;
  };

  const pass2 = (n: ts.Node): void => {
    if (ts.isVariableDeclaration(n) || ts.isParameter(n)) {
      const init = ts.isVariableDeclaration(n) ? n.initializer : undefined;
      if (ts.isObjectBindingPattern(n.name) && init && isEventLike(init)) {
        for (const el of n.name.elements) {
          const prop = literalText(el.propertyName) ?? (el.propertyName ?? el.name).getText(sf);
          const local = el.name.getText(sf);
          if (KEY_FIELDS.has(prop)) keyAliases.add(local);
          const tag = MODIFIER_TAG.get(prop);
          if (tag) modAliases.set(local, new Set([tag]));
        }
      }
      if (ts.isIdentifier(n.name) && init) {
        if (isKeyRead(init)) keyAliases.add(n.name.text);
        else if (!containsKeyRead(init)) {
          const mods = positiveModifiers(init);
          if (mods.size > 0) modAliases.set(n.name.text, mods);
        }
      }
    }
    ts.forEachChild(n, pass2);
  };
  pass2(sf);

  /* ── pass 3: the claims ────────────────────────────────────────────── */

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
      } else if (ts.isConditionalExpression(p)) {
        add(positiveModifiers(p.condition, true));
      } else if (ts.isForStatement(p) && p.condition) {
        add(positiveModifiers(p.condition, true));
      } else if (ts.isSwitchStatement(p)) {
        add(positiveModifiers(p.expression));
      } else if (ts.isExpressionStatement(p)) {
        add(positiveModifiers(p.expression));
      } else if (ts.isVariableStatement(p)) {
        add(positiveModifiers(p));
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
      } else if (ts.isCaseClause(p) && p.expression !== cur) {
        // `switch (true) { case e.ctrlKey: … }` — the discriminant is the
        // literal `true` and the assertion lives in the CLAUSE. The switch
        // arm above reads only the discriminant, so the clause's own test
        // was never consulted. Excluded when the read IS the clause
        // expression, which is the ordinary `switch (e.key) { case … }`
        // registry the switch arm already handles.
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

  const pass3 = (n: ts.Node): void => {
    if (
      ts.isSwitchStatement(n) &&
      containsKeyRead(n.expression) &&
      n.caseBlock.clauses.some((c) => ts.isCaseClause(c) && collectStrings(c.expression).length > 0)
    ) {
      switchRegistry = true;
    }
    if (isKeyRead(n)) {
      keyReads++;
      const mods = guardModifiers(n);
      if (mods.size > 0) {
        const order: ModifierTag[] = ["ctrl", "mod", "alt", "shift"];
        const modifiers = order.filter((t) => mods.has(t));
        const prefix = modifiers.join("+");
        const lits = keyLiterals(n);
        const keys = lits.length > 0 ? lits : ["?"];
        const { line } = sf.getLineAndCharacterOfPosition(n.getStart(sf));
        for (const k of keys) {
          const spelling = `${prefix}+${k.toLowerCase()}`;
          if (!claims.has(spelling)) {
            claims.set(spelling, {
              spelling,
              modifiers,
              control: modifiers.some((t) => CONTROL_TAGS.has(t)),
              text: n.parent ? n.parent.getText(sf).replace(/\s+/g, " ").slice(0, 120) : "",
              line: line + 1,
            });
          }
        }
      }
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
