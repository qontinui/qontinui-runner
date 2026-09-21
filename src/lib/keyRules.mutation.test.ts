/**
 * keyRules.mutation — the FALSIFICATION harness for the two chord-enforcement
 * mechanisms.
 *
 * ## The defect this exists for
 *
 * Iteration 12 of the manual-test loop measured that **61 of the 126 rule
 * entries** in `keyClaimScan.ts` and `keyFieldReads.ts` could be DELETED with
 * the enforcement suite still 33/33 green. `CONTROL_TAGS` — the gate on the
 * strictest property in the file — could be emptied entirely and nothing went
 * red. `MODIFIER_STATE_TAG` was dead in all six entries because
 * `positiveModifiers`' `?? "mod"` fallback is itself a `CONTROL_TAG`, so the
 * claim survived with a different spelling nobody pinned.
 *
 * That is the same class as the `ESCAPING_CLASS_COUNT = 4` that commit
 * `2dc0dde55` removed for being wrong by eleven classes: **a rule nothing
 * falsifies is a rule that can vanish silently**, and enumerating the ones a
 * reviewer can see is what has now failed eleven times in this file's history.
 *
 * So the enumeration here is MECHANICAL, not editorial. The rule atoms are
 * discovered by parsing the two modules with the TypeScript AST; for each
 * atom the module source is mutated to DELETE that atom, the mutant is
 * transpiled and instantiated in-process, and the corpus below is re-run
 * against it. An atom whose deletion leaves every corpus verdict unchanged is
 * DEAD, and this file goes red naming it.
 *
 * ## What is auto-enumerated, and what is not
 *
 * Three atom kinds, and the boundary is declared rather than implied:
 *
 *   **K1 — table entry (AUTOMATIC).** Every element of every rule table: an
 *   array literal bound to a `const`, either directly or as the sole argument
 *   of `new Set(…)` / `new Map(…)`. This is where 55 of the measured 61 dead
 *   rules lived. A NEW table, or a new entry in an existing one, is enumerated
 *   with no edit to this file — which is the property that stops rule #127
 *   from being born dead.
 *
 *   **K2 — node-kind disjunct (AUTOMATIC).** Every operand of every `||` chain
 *   whose operands are ALL `ts.isX(…)` guards or `…kind === ts.SyntaxKind.X`
 *   comparisons — i.e. every "which node shapes does this rule accept" list.
 *   `FUNCTION_LIKE`, `isStringish`, `isExit`, `unwrapReceiver` and the
 *   receiver-unwrapping `while` in `chain` are all of this shape, and all are
 *   enumerated automatically.
 *
 *   **K3 — declared single guard (HAND-WRITTEN — the residual).** A guard that
 *   is neither a table entry nor a `||` operand cannot be recognised by shape
 *   without treating every `ts.isX(…)` in a tree-walker as a rule, which would
 *   enumerate roughly a hundred atoms whose deletion merely CRASHES the walk —
 *   a crash is a red suite but not evidence the rule carries meaning. Those
 *   are therefore declared one by one in {@link DECLARED_ATOMS}, each anchored
 *   on a source substring that must occur EXACTLY ONCE, so a refactor that
 *   moves one fails loudly here instead of silently dropping the atom.
 *
 *   **This is a declared residual: a NEW guard of the K3 shape is not
 *   auto-enumerated.** It is named here rather than left to be discovered by a
 *   thirteenth iteration.
 *
 * ## Why in-process, and not `vitest run` per mutant
 *
 * D11 of the same round: `pnpm exec vitest run <file> --reporter=basic` DIES
 * before collection on vitest 4.1.5 (`Failed to load custom Reporter from
 * basic`), so a clean tree and a mutated tree are indistinguishable under it —
 * every mutation round driven through that command was reading a startup
 * error, not a verdict. A harness that shells out is a harness that can report
 * "red" for a reason that has nothing to do with the mutation.
 *
 * Here the mutant is transpiled with `ts.transpileModule` and instantiated
 * with an explicit `require` map, so nothing is resolved from disk, nothing is
 * cached, and there is no reporter, no collection and no exit status to
 * misread. {@link assertCollected} is the equivalent of the nonzero-test-count
 * check: the BASELINE verdict must be non-trivial before any mutant is graded,
 * so a corpus that silently evaluated to nothing cannot pass this file.
 */

import { readFileSync } from "fs";
import { resolve } from "path";

import * as ts from "typescript";
import { describe, expect, it } from "vitest";

import * as keyClaimScanModule from "./keyClaimScan";
import * as keyFieldReadsModule from "./keyFieldReads";

/* ── the two modules under mutation ──────────────────────────────────── */

const LIB = resolve(__dirname);
const KEY_CLAIM_SCAN = "keyClaimScan.ts";
const KEY_FIELD_READS = "keyFieldReads.ts";

const SOURCE: Record<string, string> = {
  [KEY_CLAIM_SCAN]: readFileSync(resolve(LIB, KEY_CLAIM_SCAN), "utf8"),
  [KEY_FIELD_READS]: readFileSync(resolve(LIB, KEY_FIELD_READS), "utf8"),
};

function parseModule(file: string): ts.SourceFile {
  return ts.createSourceFile(file, SOURCE[file], ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
}

/* ── atom discovery ──────────────────────────────────────────────────── */

/** One rule that can be deleted, and the edit that deletes it. */
interface Atom {
  /** `keyClaimScan.ts` / `keyFieldReads.ts`. */
  file: string;
  /** `K1` / `K2` / `K3`. */
  kind: "K1" | "K2" | "K3";
  /** The table or predicate the atom belongs to. */
  owner: string;
  /** The atom's own text — what deletion removes. */
  text: string;
  /** The mutated module source, with this atom gone. */
  mutate(): string;
}

function splice(file: string, start: number, end: number, replacement: string): string {
  const src = SOURCE[file];
  return src.slice(0, start) + replacement + src.slice(end);
}

/**
 * K1 + K2, discovered by walking the module's own AST.
 *
 * A table is not descended into: the tuples inside a `new Map([[k, v], …])`
 * are the Map's entries, not five separate two-element tables.
 */
function discoverAtoms(file: string): Atom[] {
  const sf = parseModule(file);
  const atoms: Atom[] = [];

  const tableName = (arr: ts.ArrayLiteralExpression): string | null => {
    const p = arr.parent;
    if (ts.isVariableDeclaration(p) && p.initializer === arr) return p.name.getText(sf);
    if (ts.isNewExpression(p) && p.arguments?.[0] === arr) {
      const gp = p.parent;
      if (ts.isVariableDeclaration(gp) && gp.initializer === p) return gp.name.getText(sf);
    }
    return null;
  };

  const isNodeKindGuard = (n: ts.Node): boolean => {
    const text = n.getText(sf).trim();
    return /^ts\.is\w+\(/.test(text) || /\.kind\s*===\s*ts\.SyntaxKind\./.test(text);
  };

  const walk = (n: ts.Node): void => {
    if (ts.isArrayLiteralExpression(n)) {
      const owner = tableName(n);
      if (owner !== null) {
        const elements = n.elements;
        elements.forEach((el, i) => {
          const kept = elements.filter((_, j) => j !== i).map((k) => k.getText(sf));
          atoms.push({
            file,
            kind: "K1",
            owner,
            text: el.getText(sf),
            mutate: () => splice(file, n.getStart(sf), n.getEnd(), `[${kept.join(", ")}]`),
          });
        });
        return; // a table's elements are entries, not nested tables
      }
    }

    if (ts.isBinaryExpression(n) && n.operatorToken.kind === ts.SyntaxKind.BarBarToken) {
      const parent = n.parent;
      const isChainRoot = !(
        ts.isBinaryExpression(parent) && parent.operatorToken.kind === ts.SyntaxKind.BarBarToken
      );
      if (isChainRoot) {
        const operands: ts.Node[] = [];
        const flatten = (x: ts.Node): void => {
          if (ts.isBinaryExpression(x) && x.operatorToken.kind === ts.SyntaxKind.BarBarToken) {
            flatten(x.left);
            flatten(x.right);
            return;
          }
          operands.push(x);
        };
        flatten(n);
        if (operands.length > 1 && operands.every(isNodeKindGuard)) {
          const { line } = sf.getLineAndCharacterOfPosition(n.getStart(sf));
          operands.forEach((op, i) => {
            const kept = operands.filter((_, j) => j !== i).map((k) => k.getText(sf));
            atoms.push({
              file,
              kind: "K2",
              owner: `${file}:${line + 1} node-kind disjunction`,
              text: op.getText(sf).replace(/\s+/g, " "),
              mutate: () => splice(file, n.getStart(sf), n.getEnd(), kept.join(" || ")),
            });
          });
        }
      }
    }

    ts.forEachChild(n, walk);
  };
  walk(sf);
  return atoms;
}

/**
 * K3 — guards that are neither a table entry nor a `||` operand.
 *
 * Each is anchored on a substring that must appear EXACTLY ONCE in the
 * module. A refactor that moves or re-spells one therefore reds this file
 * with the anchor that no longer resolves, rather than silently enumerating
 * one atom fewer — which is how a rule stops being falsified without anyone
 * deciding that it should.
 */
const DECLARED_ATOMS: Array<{ file: string; owner: string; find: string; replace: string }> = [
  {
    file: KEY_FIELD_READS,
    owner: "literalText: computed property name",
    find: "  if (ts.isComputedPropertyName(node)) return literalText(node.expression);\n",
    replace: "",
  },
  {
    file: KEY_FIELD_READS,
    owner: "hasGlobalKeyListener: non-null-asserted callee",
    find: "ts.isNonNullExpression(n.expression) ? n.expression.expression : n.expression",
    replace: "n.expression",
  },
  {
    file: KEY_CLAIM_SCAN,
    owner: "literalText: computed property name",
    find: "  if (ts.isComputedPropertyName(node)) return literalText(node.expression);\n",
    replace: "",
  },
];

function declaredAtoms(): Atom[] {
  return DECLARED_ATOMS.map((d) => {
    const src = SOURCE[d.file];
    const first = src.indexOf(d.find);
    const last = src.lastIndexOf(d.find);
    if (first === -1 || first !== last) {
      throw new Error(
        `declared mutation atom "${d.owner}" anchors on a substring that occurs ` +
          `${first === -1 ? 0 : 2}+ times in ${d.file}. A declared atom must resolve to exactly ` +
          `one site or it silently stops being falsified — re-anchor it on the current source.`,
      );
    }
    return {
      file: d.file,
      kind: "K3" as const,
      owner: d.owner,
      text: d.find.trim(),
      mutate: () => splice(d.file, first, first + d.find.length, d.replace),
    };
  });
}

const ATOMS: Atom[] = [
  ...discoverAtoms(KEY_CLAIM_SCAN),
  ...discoverAtoms(KEY_FIELD_READS),
  ...declaredAtoms(),
];

/* ── in-process instantiation of a mutant ────────────────────────────── */

type Loaded = Record<string, unknown>;

/**
 * Transpile and evaluate a mutated module with an EXPLICIT dependency map.
 *
 * No disk write, no dynamic `import()`, no module cache to collide with, and
 * no resolver — `require` here answers only the two specifiers these modules
 * actually use, and throws on anything else, so a mutant that silently
 * resolved a different `keyFieldReads` is impossible.
 */
function instantiate(source: string, deps: Record<string, unknown>): Loaded {
  const js = ts.transpileModule(source, {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2020,
      esModuleInterop: false,
    },
  }).outputText;
  const exports: Loaded = {};
  const req = (id: string): unknown => {
    if (id in deps) return deps[id];
    throw new Error(`mutation harness: unexpected require(${JSON.stringify(id)})`);
  };
  const factory = new Function("exports", "require", "module", js) as (
    e: Loaded,
    r: (id: string) => unknown,
    m: { exports: Loaded },
  ) => void;
  factory(exports, req, { exports });
  return exports;
}

function loadMutant(atom: Atom): Loaded {
  const mutated = atom.mutate();
  if (mutated === SOURCE[atom.file]) {
    throw new Error(`mutation for ${atom.owner} :: ${atom.text} changed nothing`);
  }
  if (mutated.length >= SOURCE[atom.file].length) {
    throw new Error(`mutation for ${atom.owner} :: ${atom.text} did not SHRINK the module`);
  }
  return instantiate(mutated, {
    typescript: ts,
    "./keyFieldReads": keyFieldReadsModule as unknown as Loaded,
  });
}

/* ── the corpora ─────────────────────────────────────────────────────── */

/**
 * Mechanism B's corpus — snippets whose claim inventory a rule of
 * `keyClaimScan.ts` decides.
 *
 * Every row exists to falsify a named rule, so a row is never "an example":
 * deleting the rule it pins must change this file's verdict on it. The
 * harness proves that per row; nothing here is asserted by reading.
 */
const B_CORPUS: string[] = [
  /* MODIFIER_TAG — the four modifier field names. */
  'if (e.ctrlKey && e.key === "z") act();',
  'if (e.metaKey && e.key === "z") act();',
  'if (e.altKey && e.key === "z") act();',
  'if (e.shiftKey && e.key === "pageup") act();',

  /* KEY_FIELDS — the four key-identity field names. */
  'if (e.ctrlKey && e.code === "KeyZ") act();',
  "if (e.ctrlKey && e.keyCode === 90) act();",
  "if (e.ctrlKey && e.which === 90) act();",

  /* STRONG_KEY_FIELDS — a key field read from a receiver that is NOT
     event-like, so the read is legible ONLY because the field's name is
     unambiguous. The modifier is asserted on a different receiver. */
  'if (state.ctrlKey && payload.code === "KeyZ") act();',
  "if (state.ctrlKey && payload.keyCode === 90) act();",
  "if (state.ctrlKey && payload.which === 90) act();",

  /* MODIFIER_STATE_TAG — each name maps to a DIFFERENT tag, and the
     `?? "mod"` fallback is what made all six dead: the claim survived under
     a spelling nothing pinned. The spelling is part of the verdict here. */
  'if (e.getModifierState("Control") && e.key === "z") act();',
  'if (e.getModifierState("Meta") && e.key === "z") act();',
  'if (e.getModifierState("OS") && e.key === "z") act();',
  'if (e.getModifierState("Alt") && e.key === "z") act();',
  'if (e.getModifierState("AltGraph") && e.key === "z") act();',
  'if (e.getModifierState("Shift") && e.key === "z") act();',

  /* CONTROL_TAGS — the `control` flag, which is what
     `globalListenerOffenders` gates on. `mod` comes from an unresolved
     predicate over the event. */
  'if (isMod(e) && e.key === "z") act();',

  /* EVENT_NAMES — the receiver is recognised ONLY by its name; the modifier
     is read from an unrelated object so nothing else can vouch for it. */
  'if (state.ctrlKey && e.key === "z") act();',
  'if (state.ctrlKey && ev.key === "z") act();',
  'if (state.ctrlKey && evt.key === "z") act();',
  'if (state.ctrlKey && event.key === "z") act();',
  'if (state.ctrlKey && ke.key === "z") act();',
  'if (state.ctrlKey && keyEvent.key === "z") act();',
  'if (state.ctrlKey && keyboardEvent.key === "z") act();',
  'if (state.ctrlKey && domEvent.key === "z") act();',
  'if (state.ctrlKey && host.nativeEvent.key === "z") act();',

  /* SANCTIONED_PREDICATES — a routed claim must NOT taint the bare-key test
     it guards. Deleting the name turns the acquittal into a `mod+…` claim. */
  'if (matchesChord(e, C)) { if (e.key === "Escape") act(); }',
  'if (matchesDigitChord(e, C)) { if (e.key === "Escape") act(); }',
  'if (isCtrlShiftChord(e, "t")) { if (e.key === "Escape") act(); }',

  /* MEMBERSHIP_METHODS — the key value is the ARGUMENT and the literals are
     on the receiver. */
  'if (e.ctrlKey && ["z"].includes(e.key)) act();',
  'if (e.ctrlKey && new Set(["z"]).has(e.key)) act();',
  'if (e.ctrlKey && ["z"].indexOf(e.key) >= 0) act();',
  'if (e.ctrlKey && ["z"].lastIndexOf(e.key) >= 0) act();',

  /* RECEIVER_METHODS — the key value is the RECEIVER and the literals are in
     the arguments. */
  'if (e.ctrlKey && e.key.startsWith("z")) act();',
  'if (e.ctrlKey && e.key.endsWith("z")) act();',
  'if (e.ctrlKey && e.key.localeCompare("z") === 0) act();',
  'if (e.ctrlKey && e.key.match("z")) act();',
  'if (e.ctrlKey && e.key.matchAll("z")) act();',
  'if (e.ctrlKey && e.key.search("z")) act();',
  'if (e.ctrlKey && e.key.includes("z")) act();',
  'if (e.ctrlKey && e.key.indexOf("z") >= 0) act();',
  "if (e.ctrlKey && e.key.charCodeAt(1) === 90) act();",
  "if (e.ctrlKey && e.key.codePointAt(1) === 90) act();",
  'if (e.ctrlKey && e.key.normalize("NFC") === "z") act();',

  /* APPLIED_METHODS — the literals are on the receiver and the key value is
     the argument, but the receiver is a pattern rather than a collection. */
  "if (e.ctrlKey && /^z$/.test(e.key)) act();",
  "if (e.ctrlKey && /^z$/.exec(e.key)) act();",

  /* COMPARISON_OPS — every operator that can carry a key literal. */
  'if (e.ctrlKey && e.key == "z") act();',
  'if (e.ctrlKey && e.key != "z") act();',
  'if (e.ctrlKey && e.key !== "z") act();',
  'if (e.ctrlKey && e.key > "z") act();',
  'if (e.ctrlKey && e.key >= "z") act();',
  'if (e.ctrlKey && e.key < "z") act();',
  'if (e.ctrlKey && e.key <= "z") act();',

  /* ASSIGNMENT_OPS — the modifier hoisted into a pre-declared `let`. */
  'let m1 = false; m1 = e.ctrlKey; if (m1 && e.key === "z") act();',
  'let m2 = false; m2 ||= e.ctrlKey; if (m2 && e.key === "z") act();',
  'let m3 = true; m3 &&= e.ctrlKey; if (m3 && e.key === "z") act();',
  'let m4: boolean | undefined; m4 ??= e.ctrlKey; if (m4 && e.key === "z") act();',

  /* FUNCTION_LIKE — a modifier read inside a nested function belongs to a
     DIFFERENT press, so it must not gate the bare-key test beside it. One
     row per function shape the language has. */
  'if (foo(function () { return e.ctrlKey; })) { if (e.key === "z") act(); }',
  'if (foo(() => e.ctrlKey)) { if (e.key === "z") act(); }',
  'if (foo({ m() { return e.ctrlKey; } })) { if (e.key === "z") act(); }',
  'if (foo({ get m() { return e.ctrlKey; } })) { if (e.key === "z") act(); }',
  'if (foo({ set m(v: boolean) { act(e.ctrlKey); } })) { if (e.key === "z") act(); }',
  'if (foo(class { constructor() { act(e.ctrlKey); } })) { if (e.key === "z") act(); }',
  'switch (e.key) { case "z": { function g() { act(e.ctrlKey); } act(); } break; }',

  /* literalText / unwrapReceiver — the spellings of a literal and of a
     receiver that change nothing about what is read. */
  "if (e.ctrlKey && e.key === `z`) act();",
  'if (state.ctrlKey && (e).key === "z") act();',
  'if (state.ctrlKey && e!.key === "z") act();',
  'if (state.ctrlKey && (e as KeyboardEvent).key === "z") act();',
  'if (state.ctrlKey && (e satisfies KeyboardEvent).key === "z") act();',

  /* pass 1 — a receiver named nothing like an event, recognised by its TYPE
     ANNOTATION, in both positions that can carry one. */
  'const h1 = (kev: KeyboardEvent) => { if (state.ctrlKey && kev.key === "z") act(); };',
  'const kev2: KeyboardEvent = getE(); if (state.ctrlKey && kev2.key === "z") act();',

  /* isExit / inheritedGuards — the early-return guard, in every exit shape
     and every statement-list container. */
  'function h2() { if (!e.ctrlKey) return; if (e.key === "z") act(); }',
  'while (x) { if (!e.ctrlKey) continue; if (e.key === "z") act(); }',
  'while (x) { if (!e.ctrlKey) break; if (e.key === "z") act(); }',
  'if (!e.ctrlKey) throw new Error("x"); if (e.key === "z") act();',
  'switch (v) { case 1: if (!e.ctrlKey) break; if (e.key === "z") act(); }',
  'switch (v) { default: if (!e.ctrlKey) break; if (e.key === "z") act(); }',

  /* pass 2 — the destructured event, in both declaration positions. */
  'const { key: k9, ctrlKey: c9 } = e; if (c9 && k9 === "z") act();',
  'const onK = ({ key: k8, ctrlKey: c8 }: KeyboardEvent) => { if (c8 && k8 === "z") act(); };',

  /* branchGuards — the modifier that refines a key test from BELOW. */
  'if (e.key === "z") { if (e.ctrlKey) act(); }',
  'if (e.key === "z") { while (e.ctrlKey) act(); }',
  'if (e.key === "z") { do { act(); } while (e.ctrlKey); }',

  /* guardModifiers — the enclosing conditions, in every statement shape. */
  'if (e.ctrlKey) { if (e.key === "z") act(); }',
  'while (e.ctrlKey) { if (e.key === "z") act(); }',
  'do { if (e.key === "z") act(); } while (e.ctrlKey);',
  'const hit9 = e.ctrlKey && e.key === "z";',
  'class C9 { hit = e.ctrlKey && e.key === "z"; }',

  /* element access — a field addressed by an expression this pass cannot
     evaluate is counted as a possible key read; one addressed by a literal
     that is not a key field is not. */
  'const F9 = "key"; if (e.ctrlKey && e[F9] === "z") act();',
  'if (e.ctrlKey && e["ariaLabel"] === "z") act();',

  /* literalText's computed-property arm — a binding whose property name is
     a COMPUTED literal. Without it the property reads as the source text
     `["key"]`, which is on no field roster, and both aliases are lost. */
  'const { ["key"]: k7, ["ctrlKey"]: c7 } = e; if (c7 && k7 === "z") act();',
];

/**
 * Mechanism A's corpus — snippets whose field-read verdict or global-listener
 * verdict a rule of `keyFieldReads.ts` decides.
 */
const A_CORPUS: string[] = [
  /* MODIFIER_FIELDS — one row per field name. */
  "if (e.altKey) act();",
  "if (e.ctrlKey) act();",
  'e.getModifierState("Control");',
  "if (e.keyCode === 90) act();",
  "if (e.metaKey) act();",
  "if (e.shiftKey) act();",
  "if (e.which === 90) act();",

  /* AMBIGUOUS_KEY_FIELDS. */
  'if (e.key === "z") act();',
  'if (e.code === "KeyZ") act();',

  /* literalText / isCallee — the read spellings that are not a plain
     property access, and the two call positions that are NOT reads. */
  'const { ["key"]: k } = e;',
  "const { [`code`]: c } = e;",
  "localStorage.key(0);",
  "new Registry.key(0);",

  /* isStringish — R4 and R5, across every literal node that carries author
     text. A template's head, middle and tail are separate nodes, and the
     field name lives in whichever one the author happened to write it in. */
  'const MODS = ["metaKey"];',
  "eval(`e.ctrlKey ${x}`);",
  "eval(`${a}e.altKey${b}`);",
  "eval(`${a}e.shiftKey`);",

  /* GLOBAL_TARGETS. */
  'window.addEventListener("keydown", h);',
  'document.addEventListener("keydown", h);',
  'globalThis.addEventListener("keydown", h);',
  'self.addEventListener("keydown", h);',

  /* GLOBAL_CHAINS — app-wide receivers whose trailing name is not a global. */
  'document.body.addEventListener("keydown", h);',
  'document.documentElement.addEventListener("keydown", h);',
  'window.document.addEventListener("keydown", h);',
  'window.document.body.addEventListener("keydown", h);',

  /* ON_KEY_HANDLERS — a registration with no call in it at all. */
  "window.onkeydown = h;",
  "window.onkeyup = h;",
  "window.onkeypress = h;",

  /* chain — the receiver spellings that change nothing about the target. */
  '(window).addEventListener("keydown", h);',
  'window!.addEventListener("keydown", h);',
  'window.addEventListener!("keydown", h);',
  '(window as EventTarget).addEventListener("keydown", h);',
  '(window satisfies Window).addEventListener("keydown", h);',
  '(<EventTarget>window).addEventListener("keydown", h);',

  /* eventName — the ARGUMENT spellings that change nothing about the event,
     and the concatenation fold. */
  'window.addEventListener(("keydown"), h);',
  'window.addEventListener("keydown"!, h);',
  'window.addEventListener("keydown" as string, h);',
  'window.addEventListener("keydown" satisfies string, h);',
  'window.addEventListener(<string>"keydown", h);',
  'window.addEventListener("key" + "down", h);',

  /* CHORD_MODIFIER_WORDS — mechanism C's C1. One row per word, because the
     vocabulary IS the rule: a library this repo has never heard of is caught
     only if the modifier word it spells is on the list. `command` and
     `commandorcontrol` (and `cmd` / `cmdorctrl`, `ctrl` / `control`) need
     separate rows — the longer alternative matches the shorter one's row. */
  'bind("alt+j", act);',
  'bind("cmd+j", act);',
  'bind("cmdorctrl+j", act);',
  'bind("command+j", act);',
  'bind("commandorcontrol+j", act);',
  'bind("control+j", act);',
  'bind("ctrl+j", act);',
  'bind("meta+j", act);',
  'bind("mod+j", act);',
  'bind("option+j", act);',
  'bind("shift+f3", act);',
  'bind("super+j", act);',
  'bind("win+j", act);',

  /* Mechanism C's clean side — the bound on C1. */
  'const sum = "1 + 2"; const label = "a+b"; const word = "altitude+x";',

  /* KEYBINDING_CALLS / KEYBINDING_NAMESPACES — mechanism C's C3, the
     enumerative half, which is exactly why every entry needs a row. */
  "ed.addAction({ keybindings: [2048 | 42] });",
  "ed.addCommand(2048 | 42, act);",
  "svc.addKeybinding(2048 | 42, act);",
  "svc.registerKeybinding(2048 | 42, act);",
  "const a1 = monaco.KeyCode.KeyJ;",
  "const a2 = monaco.KeyMod.CtrlCmd;",

  /* accessKey — the platform accelerator, in the two non-JSX spellings. */
  'el.accessKey = "j";',
  'el.setAttribute("accesskey", "j");',
];

/* ── verdicts ────────────────────────────────────────────────────────── */

interface ScanLike {
  claims: Array<{ spelling: string; modifiers: string[]; control: boolean }>;
  keyReads: number;
  switchRegistry: boolean;
}

type ScanFn = (text: string, fileName?: string) => ScanLike;

/**
 * Mechanism B's verdict on one snippet.
 *
 * The SPELLING and the `control` flag are part of it, not just "was anything
 * claimed". That is deliberate: `MODIFIER_STATE_TAG` and `CONTROL_TAGS` were
 * dead precisely because a coarser verdict ("some claim exists") could not
 * see them. `record`'s `?? "mod"` fallback keeps the claim alive under a
 * different name, and a `control` flag flipped to false is what silently
 * disarms `globalListenerOffenders`.
 */
function verdictB(mod: Loaded, snippet: string): string {
  const scan = mod.scanKeyClaims as ScanFn;
  try {
    const r = scan(snippet, "probe.tsx");
    const claims = r.claims
      .map((c) => `${c.spelling}[${c.modifiers.join(",")}${c.control ? "!" : ""}]`)
      .join(" ");
    return `${claims || "-"} #${r.keyReads}${r.switchRegistry ? " sw" : ""}`;
  } catch (err) {
    return `THREW: ${(err as Error).message}`;
  }
}

interface ReadsLike {
  modifier: string[];
  ambiguous: string[];
}

interface NonDomLike {
  chordStrings: string[];
  accessKeys: string[];
  keybindingApis: string[];
}

/**
 * Mechanism A's verdict on one snippet: both field tiers, the listener grade,
 * and mechanism C's claim set.
 *
 * Parsed as `probe.ts`, never `.tsx`: a `<string>"keydown"` cast is a
 * legitimate spelling of a listener argument and TSX reads it as the start of
 * a JSX element. Mechanism C's one JSX rule (`accessKey` as an attribute) is
 * not a rule TABLE and so is not enumerated here; it is pinned in the
 * enforcement suite, where the snippet is parsed as TSX.
 */
function verdictA(mod: Loaded, snippet: string): string {
  const parse = mod.parseSource as (t: string, f: string) => ts.SourceFile;
  const reads = mod.findKeyFieldReads as (sf: ts.SourceFile) => ReadsLike;
  const listener = mod.hasGlobalKeyListener as (sf: ts.SourceFile) => boolean;
  const nonDom = mod.findNonDomChordClaims as (sf: ts.SourceFile) => NonDomLike;
  try {
    const sf = parse(snippet, "probe.ts");
    const r = reads(sf);
    const c = nonDom(sf);
    const part = (label: string, xs: string[]): string =>
      xs.length === 0 ? "" : ` ${label}:${xs.join(",")}`;
    return (
      `${r.modifier.join(",") || "-"} / ${r.ambiguous.join(",") || "-"}` +
      (listener(sf) ? " LISTENER" : "") +
      part("chord", c.chordStrings) +
      part("access", c.accessKeys) +
      part("api", c.keybindingApis)
    ).trim();
  } catch (err) {
    return `THREW: ${(err as Error).message}`;
  }
}

function signature(mod: Loaded, file: string): string[] {
  return file === KEY_CLAIM_SCAN
    ? B_CORPUS.map((s) => `${s} => ${verdictB(mod, s)}`)
    : A_CORPUS.map((s) => `${s} => ${verdictA(mod, s)}`);
}

/**
 * The corpus's verdict, PINNED.
 *
 * Generated from the live modules, and regenerated deliberately: the failure
 * message of "pins the corpus verdict" prints the new value. It exists
 * because falsification and deletion are different properties — see that
 * test's docstring.
 */
const PINNED: Record<string, string[]> = {
  [KEY_CLAIM_SCAN]: [
    'if (e.ctrlKey && e.key === "z") act(); => ctrl+z[ctrl!] #1',
    'if (e.metaKey && e.key === "z") act(); => ctrl+z[ctrl!] #1',
    'if (e.altKey && e.key === "z") act(); => alt+z[alt!] #1',
    'if (e.shiftKey && e.key === "pageup") act(); => shift+pageup[shift] #1',
    'if (e.ctrlKey && e.code === "KeyZ") act(); => ctrl+keyz[ctrl!] #1',
    "if (e.ctrlKey && e.keyCode === 90) act(); => ctrl+90[ctrl!] #1",
    "if (e.ctrlKey && e.which === 90) act(); => ctrl+90[ctrl!] #1",
    'if (state.ctrlKey && payload.code === "KeyZ") act(); => ctrl+keyz[ctrl!] #1',
    "if (state.ctrlKey && payload.keyCode === 90) act(); => ctrl+90[ctrl!] #1",
    "if (state.ctrlKey && payload.which === 90) act(); => ctrl+90[ctrl!] #1",
    'if (e.getModifierState("Control") && e.key === "z") act(); => ctrl+z[ctrl!] #1',
    'if (e.getModifierState("Meta") && e.key === "z") act(); => ctrl+z[ctrl!] #1',
    'if (e.getModifierState("OS") && e.key === "z") act(); => ctrl+z[ctrl!] #1',
    'if (e.getModifierState("Alt") && e.key === "z") act(); => alt+z[alt!] #1',
    'if (e.getModifierState("AltGraph") && e.key === "z") act(); => alt+z[alt!] #1',
    'if (e.getModifierState("Shift") && e.key === "z") act(); => shift+z[shift] #1',
    'if (isMod(e) && e.key === "z") act(); => mod+z[mod!] #1',
    'if (state.ctrlKey && e.key === "z") act(); => ctrl+z[ctrl!] #1',
    'if (state.ctrlKey && ev.key === "z") act(); => ctrl+z[ctrl!] #1',
    'if (state.ctrlKey && evt.key === "z") act(); => ctrl+z[ctrl!] #1',
    'if (state.ctrlKey && event.key === "z") act(); => ctrl+z[ctrl!] #1',
    'if (state.ctrlKey && ke.key === "z") act(); => ctrl+z[ctrl!] #1',
    'if (state.ctrlKey && keyEvent.key === "z") act(); => ctrl+z[ctrl!] #1',
    'if (state.ctrlKey && keyboardEvent.key === "z") act(); => ctrl+z[ctrl!] #1',
    'if (state.ctrlKey && domEvent.key === "z") act(); => ctrl+z[ctrl!] #1',
    'if (state.ctrlKey && host.nativeEvent.key === "z") act(); => ctrl+z[ctrl!] #1',
    'if (matchesChord(e, C)) { if (e.key === "Escape") act(); } => - #1',
    'if (matchesDigitChord(e, C)) { if (e.key === "Escape") act(); } => - #1',
    'if (isCtrlShiftChord(e, "t")) { if (e.key === "Escape") act(); } => - #1',
    'if (e.ctrlKey && ["z"].includes(e.key)) act(); => ctrl+z[ctrl!] #1',
    'if (e.ctrlKey && new Set(["z"]).has(e.key)) act(); => ctrl+z[ctrl!] #1',
    'if (e.ctrlKey && ["z"].indexOf(e.key) >= 0) act(); => ctrl+z[ctrl!] #1',
    'if (e.ctrlKey && ["z"].lastIndexOf(e.key) >= 0) act(); => ctrl+z[ctrl!] #1',
    'if (e.ctrlKey && e.key.startsWith("z")) act(); => ctrl+z[ctrl!] #1',
    'if (e.ctrlKey && e.key.endsWith("z")) act(); => ctrl+z[ctrl!] #1',
    'if (e.ctrlKey && e.key.localeCompare("z") === 0) act(); => ctrl+z[ctrl!] #1',
    'if (e.ctrlKey && e.key.match("z")) act(); => ctrl+z[ctrl!] #1',
    'if (e.ctrlKey && e.key.matchAll("z")) act(); => ctrl+z[ctrl!] #1',
    'if (e.ctrlKey && e.key.search("z")) act(); => ctrl+z[ctrl!] #1',
    'if (e.ctrlKey && e.key.includes("z")) act(); => ctrl+z[ctrl!] #1',
    'if (e.ctrlKey && e.key.indexOf("z") >= 0) act(); => ctrl+z[ctrl!] #1',
    "if (e.ctrlKey && e.key.charCodeAt(1) === 90) act(); => ctrl+1[ctrl!] #1",
    "if (e.ctrlKey && e.key.codePointAt(1) === 90) act(); => ctrl+1[ctrl!] #1",
    'if (e.ctrlKey && e.key.normalize("NFC") === "z") act(); => ctrl+nfc[ctrl!] #1',
    "if (e.ctrlKey && /^z$/.test(e.key)) act(); => ctrl+/^z$/[ctrl!] #1",
    "if (e.ctrlKey && /^z$/.exec(e.key)) act(); => ctrl+/^z$/[ctrl!] #1",
    'if (e.ctrlKey && e.key == "z") act(); => ctrl+z[ctrl!] #1',
    'if (e.ctrlKey && e.key != "z") act(); => ctrl+z[ctrl!] #1',
    'if (e.ctrlKey && e.key !== "z") act(); => ctrl+z[ctrl!] #1',
    'if (e.ctrlKey && e.key > "z") act(); => ctrl+z[ctrl!] #1',
    'if (e.ctrlKey && e.key >= "z") act(); => ctrl+z[ctrl!] #1',
    'if (e.ctrlKey && e.key < "z") act(); => ctrl+z[ctrl!] #1',
    'if (e.ctrlKey && e.key <= "z") act(); => ctrl+z[ctrl!] #1',
    'let m1 = false; m1 = e.ctrlKey; if (m1 && e.key === "z") act(); => ctrl+z[ctrl!] #1',
    'let m2 = false; m2 ||= e.ctrlKey; if (m2 && e.key === "z") act(); => ctrl+z[ctrl!] #1',
    'let m3 = true; m3 &&= e.ctrlKey; if (m3 && e.key === "z") act(); => ctrl+z[ctrl!] #1',
    'let m4: boolean | undefined; m4 ??= e.ctrlKey; if (m4 && e.key === "z") act(); => ctrl+z[ctrl!] #1',
    'if (foo(function () { return e.ctrlKey; })) { if (e.key === "z") act(); } => - #1',
    'if (foo(() => e.ctrlKey)) { if (e.key === "z") act(); } => - #1',
    'if (foo({ m() { return e.ctrlKey; } })) { if (e.key === "z") act(); } => - #1',
    'if (foo({ get m() { return e.ctrlKey; } })) { if (e.key === "z") act(); } => - #1',
    'if (foo({ set m(v: boolean) { act(e.ctrlKey); } })) { if (e.key === "z") act(); } => - #1',
    'if (foo(class { constructor() { act(e.ctrlKey); } })) { if (e.key === "z") act(); } => - #1',
    'switch (e.key) { case "z": { function g() { act(e.ctrlKey); } act(); } break; } => - #1 sw',
    "if (e.ctrlKey && e.key === `z`) act(); => ctrl+z[ctrl!] #1",
    'if (state.ctrlKey && (e).key === "z") act(); => ctrl+z[ctrl!] #1',
    'if (state.ctrlKey && e!.key === "z") act(); => ctrl+z[ctrl!] #1',
    'if (state.ctrlKey && (e as KeyboardEvent).key === "z") act(); => ctrl+z[ctrl!] #1',
    'if (state.ctrlKey && (e satisfies KeyboardEvent).key === "z") act(); => ctrl+z[ctrl!] #1',
    'const h1 = (kev: KeyboardEvent) => { if (state.ctrlKey && kev.key === "z") act(); }; => ctrl+z[ctrl!] #1',
    'const kev2: KeyboardEvent = getE(); if (state.ctrlKey && kev2.key === "z") act(); => ctrl+z[ctrl!] #1',
    'function h2() { if (!e.ctrlKey) return; if (e.key === "z") act(); } => ctrl+z[ctrl!] #1',
    'while (x) { if (!e.ctrlKey) continue; if (e.key === "z") act(); } => ctrl+z[ctrl!] #1',
    'while (x) { if (!e.ctrlKey) break; if (e.key === "z") act(); } => ctrl+z[ctrl!] #1',
    'if (!e.ctrlKey) throw new Error("x"); if (e.key === "z") act(); => ctrl+z[ctrl!] #1',
    'switch (v) { case 1: if (!e.ctrlKey) break; if (e.key === "z") act(); } => ctrl+z[ctrl!] #1',
    'switch (v) { default: if (!e.ctrlKey) break; if (e.key === "z") act(); } => ctrl+z[ctrl!] #1',
    'const { key: k9, ctrlKey: c9 } = e; if (c9 && k9 === "z") act(); => ctrl+z[ctrl!] #1',
    'const onK = ({ key: k8, ctrlKey: c8 }: KeyboardEvent) => { if (c8 && k8 === "z") act(); }; => ctrl+z[ctrl!] #1',
    'if (e.key === "z") { if (e.ctrlKey) act(); } => ctrl+z[ctrl!] #1',
    'if (e.key === "z") { while (e.ctrlKey) act(); } => ctrl+z[ctrl!] #1',
    'if (e.key === "z") { do { act(); } while (e.ctrlKey); } => ctrl+z[ctrl!] #1',
    'if (e.ctrlKey) { if (e.key === "z") act(); } => ctrl+z[ctrl!] #1',
    'while (e.ctrlKey) { if (e.key === "z") act(); } => ctrl+z[ctrl!] #1',
    'do { if (e.key === "z") act(); } while (e.ctrlKey); => ctrl+z[ctrl!] #1',
    'const hit9 = e.ctrlKey && e.key === "z"; => ctrl+z[ctrl!] #1',
    'class C9 { hit = e.ctrlKey && e.key === "z"; } => ctrl+z[ctrl!] #1',
    'const F9 = "key"; if (e.ctrlKey && e[F9] === "z") act(); => ctrl+mod+z[ctrl,mod!] #1',
    'if (e.ctrlKey && e["ariaLabel"] === "z") act(); => - #0',
    'const { ["key"]: k7, ["ctrlKey"]: c7 } = e; if (c7 && k7 === "z") act(); => ctrl+z[ctrl!] #1',
  ],
  [KEY_FIELD_READS]: [
    "if (e.altKey) act(); => altKey / -",
    "if (e.ctrlKey) act(); => ctrlKey / -",
    'e.getModifierState("Control"); => getModifierState / -',
    "if (e.keyCode === 90) act(); => keyCode / -",
    "if (e.metaKey) act(); => metaKey / -",
    "if (e.shiftKey) act(); => shiftKey / -",
    "if (e.which === 90) act(); => which / -",
    'if (e.key === "z") act(); => - / key',
    'if (e.code === "KeyZ") act(); => - / code',
    'const { ["key"]: k } = e; => - / key',
    "const { [`code`]: c } = e; => - / code",
    "localStorage.key(0); => - / -",
    "new Registry.key(0); => - / -",
    'const MODS = ["metaKey"]; => metaKey / -',
    "eval(`e.ctrlKey ${x}`); => ctrlKey / -",
    "eval(`${a}e.altKey${b}`); => altKey / -",
    "eval(`${a}e.shiftKey`); => shiftKey / -",
    'window.addEventListener("keydown", h); => - / - LISTENER',
    'document.addEventListener("keydown", h); => - / - LISTENER',
    'globalThis.addEventListener("keydown", h); => - / - LISTENER',
    'self.addEventListener("keydown", h); => - / - LISTENER',
    'document.body.addEventListener("keydown", h); => - / - LISTENER',
    'document.documentElement.addEventListener("keydown", h); => - / - LISTENER',
    'window.document.addEventListener("keydown", h); => - / - LISTENER',
    'window.document.body.addEventListener("keydown", h); => - / - LISTENER',
    "window.onkeydown = h; => - / - LISTENER",
    "window.onkeyup = h; => - / - LISTENER",
    "window.onkeypress = h; => - / - LISTENER",
    '(window).addEventListener("keydown", h); => - / - LISTENER',
    'window!.addEventListener("keydown", h); => - / - LISTENER',
    'window.addEventListener!("keydown", h); => - / - LISTENER',
    '(window as EventTarget).addEventListener("keydown", h); => - / - LISTENER',
    '(window satisfies Window).addEventListener("keydown", h); => - / - LISTENER',
    '(<EventTarget>window).addEventListener("keydown", h); => - / - LISTENER',
    'window.addEventListener(("keydown"), h); => - / - LISTENER',
    'window.addEventListener("keydown"!, h); => - / - LISTENER',
    'window.addEventListener("keydown" as string, h); => - / - LISTENER',
    'window.addEventListener("keydown" satisfies string, h); => - / - LISTENER',
    'window.addEventListener(<string>"keydown", h); => - / - LISTENER',
    'window.addEventListener("key" + "down", h); => - / - LISTENER',
    'bind("alt+j", act); => - / - chord:alt+j',
    'bind("cmd+j", act); => - / - chord:cmd+j',
    'bind("cmdorctrl+j", act); => - / - chord:cmdorctrl+j',
    'bind("command+j", act); => - / - chord:command+j',
    'bind("commandorcontrol+j", act); => - / - chord:commandorcontrol+j',
    'bind("control+j", act); => - / - chord:control+j',
    'bind("ctrl+j", act); => - / - chord:ctrl+j',
    'bind("meta+j", act); => - / - chord:meta+j',
    'bind("mod+j", act); => - / - chord:mod+j',
    'bind("option+j", act); => - / - chord:option+j',
    'bind("shift+f3", act); => - / - chord:shift+f3',
    'bind("super+j", act); => - / - chord:super+j',
    'bind("win+j", act); => - / - chord:win+j',
    'const sum = "1 + 2"; const label = "a+b"; const word = "altitude+x"; => - / -',
    "ed.addAction({ keybindings: [2048 | 42] }); => - / - api:addAction",
    "ed.addCommand(2048 | 42, act); => - / - api:addCommand",
    "svc.addKeybinding(2048 | 42, act); => - / - api:addKeybinding",
    "svc.registerKeybinding(2048 | 42, act); => - / - api:registerKeybinding",
    "const a1 = monaco.KeyCode.KeyJ; => - / - api:KeyCode",
    "const a2 = monaco.KeyMod.CtrlCmd; => - / - api:KeyMod",
    'el.accessKey = "j"; => - / - access:j',
    'el.setAttribute("accesskey", "j"); => - / - access:?',
  ],
};

const BASELINE: Record<string, string[]> = {
  [KEY_CLAIM_SCAN]: signature(keyClaimScanModule as unknown as Loaded, KEY_CLAIM_SCAN),
  [KEY_FIELD_READS]: signature(keyFieldReadsModule as unknown as Loaded, KEY_FIELD_READS),
};

/**
 * The "did collection actually happen" check, which is what D11 says every
 * mutation harness must assert instead of trusting an exit status.
 *
 * A baseline in which nothing was claimed and nothing was read is a corpus
 * that evaluated to nothing — under which EVERY mutant looks identical to the
 * baseline and every rule looks dead. Assert the baseline is substantive
 * before grading a single mutant.
 */
function assertCollected(): void {
  const b = BASELINE[KEY_CLAIM_SCAN].filter((row) => !/ => - #/.test(row));
  const a = BASELINE[KEY_FIELD_READS].filter((row) => !/ => - \/ -$/.test(row));
  const listeners = BASELINE[KEY_FIELD_READS].filter((row) => row.includes("LISTENER"));
  const chords = BASELINE[KEY_FIELD_READS].filter((row) => /(chord|access|api):/.test(row));
  expect(BASELINE[KEY_CLAIM_SCAN].some((r) => r.includes("THREW"))).toBe(false);
  expect(BASELINE[KEY_FIELD_READS].some((r) => r.includes("THREW"))).toBe(false);
  expect(b.length, "baseline B corpus must actually claim chords").toBeGreaterThan(60);
  expect(a.length, "baseline A corpus must actually read fields").toBeGreaterThan(12);
  expect(listeners.length, "baseline A corpus must actually find listeners").toBeGreaterThan(10);
  expect(chords.length, "baseline A corpus must actually find non-DOM chords").toBeGreaterThan(15);
}

/* ── the properties ──────────────────────────────────────────────────── */

describe("rule falsification — every rule entry is killed by a corpus row", () => {
  it("collects a substantive baseline before grading any mutant", () => {
    assertCollected();
  });

  /**
   * The DELETION half, and it is not the same property as falsification.
   *
   * The mutation pass below asks "does every rule that EXISTS carry its
   * weight?" — it mutates a copy and compares against the live module, so a
   * rule someone simply DELETES from the real source stops being enumerated
   * and nothing reds. Measured, not reasoned: deleting `"startsWith"` from
   * `RECEIVER_METHODS` left both this file and the enforcement suite green,
   * because every assertion downstream of it only asked whether SOME claim
   * was found and `ctrl+?` is some claim.
   *
   * {@link PINNED} closes that. It is the corpus's verdict, checked in, one
   * readable line per row — so a rule that disappears moves a spelling and
   * this goes red naming the row. Regenerate it deliberately when a verdict
   * is MEANT to change; the failure output is the new value.
   */
  it("pins the corpus verdict, so DELETING a rule reds this file too", () => {
    expect(BASELINE[KEY_CLAIM_SCAN]).toEqual(PINNED[KEY_CLAIM_SCAN]);
    expect(BASELINE[KEY_FIELD_READS]).toEqual(PINNED[KEY_FIELD_READS]);
  });

  it("enumerates the rule tables mechanically, not by hand", () => {
    // Anti-vacuity. A discovery pass that found nothing would report every
    // rule as falsified without running a single mutation.
    const tables = new Set(ATOMS.filter((a) => a.kind === "K1").map((a) => a.owner));
    expect(tables.has("KEY_FIELDS")).toBe(true);
    expect(tables.has("CONTROL_TAGS")).toBe(true);
    expect(tables.has("MODIFIER_STATE_TAG")).toBe(true);
    expect(tables.has("RECEIVER_METHODS")).toBe(true);
    expect(tables.has("MODIFIER_FIELDS")).toBe(true);
    expect(tables.has("GLOBAL_CHAINS")).toBe(true);
    expect(ATOMS.filter((a) => a.kind === "K1").length).toBeGreaterThan(60);
    expect(ATOMS.filter((a) => a.kind === "K2").length).toBeGreaterThan(30);
    expect(ATOMS.filter((a) => a.kind === "K3").length).toBe(DECLARED_ATOMS.length);
  });

  it("goes RED for every single rule entry when that entry is deleted", () => {
    const dead: string[] = [];
    for (const atom of ATOMS) {
      const mutant = loadMutant(atom);
      const mutated = signature(mutant, atom.file);
      const base = BASELINE[atom.file];
      const changed = mutated.some((row, i) => row !== base[i]);
      if (!changed) {
        dead.push(`${atom.kind} ${atom.file} ${atom.owner} :: ${atom.text}`);
      }
    }
    expect(
      dead,
      `these rule entries can be DELETED with every corpus row still agreeing — they are ` +
        `unfalsifiable. Either add a corpus row above that goes red when the entry is gone, ` +
        `or delete the entry from the module. Do not "declare" one falsified without a row.`,
    ).toEqual([]);
  }, 600_000);
});
