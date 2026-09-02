/**
 * actionSurfaces — a TypeScript-AST enumeration of EVERY UI Bridge action
 * surface in `src/`, and whether each one can be reached with an unvalidated
 * argument bag.
 *
 * ## Why an enumeration and not another sweep
 *
 * Twelve rounds of a manual-test loop on this app have produced 108 defects,
 * and one failure has recurred ELEVEN times: *a fix passed review because the
 * defective shape lived somewhere the fix did not reach*. Six of those eleven
 * were inside the fix meant to close the class.
 *
 * The most recent is exact. PR #1301 hardened four action handlers against an
 * unvalidated bag and left three siblings open IN THE SAME `actions: [...]`
 * ARRAY — while its own docstring asserted those siblings "route through
 * `callRegistry` → `bindDirect` and so refuse `{context: {}}` before any
 * effect". Measured on the page, one batch, the same input `5`:
 *
 *     create-ai-session(5)   → throws, wire [], 0 created      ← the fixed one
 *     create-plain(5)        → success: true, terminal_create, live 0 → 1
 *     create-best-account(5) → success: true, terminal_create +
 *                              build_ai_launch_command + a terminal_write of
 *                              `claude --session-id … --config-dir …`
 *
 * Enumerating the instances you can see is what failed eleven times. So the
 * question this module answers is not "are these four fixed" but "how many
 * action surfaces exist in this tree, and which of them can read a bag nobody
 * checked" — asked of the AST, so a surface added tomorrow is asked too.
 *
 * ## What counts as a surface
 *
 * Three passes, deliberately overlapping. Any ONE of them alone has a hole
 * the other two cover:
 *
 *   - **Position** — an entry of an `actions: [...]` array literal, or a value
 *     of a `customActions: {...}` object literal. This is where a surface is
 *     REGISTERED. Its hole: a surface built by a factory in another file.
 *   - **Shape** — ANY object literal in the tree carrying both an `id` and a
 *     `handler`, wherever it sits. This is what a surface IS. It catches the
 *     factory, the helper, the object returned from a hook — the "somewhere
 *     the fix did not reach". Its hole: a surface whose object literal is
 *     produced by a wrapper and never written out.
 *   - **Guarded** — every call to `guardedAction` / `guardedCustomAction`,
 *     anywhere. This is how a surface is supposed to be written, and it is
 *     what makes the guarded ones countable rather than merely absent from the
 *     violation list.
 *
 * A surface is a VIOLATION when it is written as an object literal whose
 * `handler` can read the caller's bag:
 *
 *   - an inline function of arity ≥ 1 — it receives `params` and is on its own
 *     with them, which is the shape every one of D1–D5 was written in;
 *   - a `handler` that is not an inline function at all (an identifier, a
 *     call, a conditional) — its arity is not decidable here, and "not
 *     decidable" must fail CLOSED. The fix is to write it as a
 *     `guardedAction`, which is decidable by construction.
 *
 * An arity-0 handler is safe STRUCTURALLY, not by inspection: it has no
 * binding for `params`, so no bag can reach it whatever its body does.
 *
 * Arity must be read from the AST and NOWHERE ELSE. `useUIComponent` re-wraps
 * every registered action in a stable two-argument forwarder before the SDK
 * ever sees it (`dist/react/index.mjs`, "Forwards BOTH arguments"), so at
 * runtime EVERY handler reports `length === 2` regardless of what its author
 * wrote. A runtime-arity version of this check would classify all 165 surfaces
 * as parameterised and be useless in the same breath as looking rigorous.
 *
 * ### What "safe" does NOT mean here — a measured, stated residual
 *
 * It means no argument can INFLUENCE the effect. It does NOT mean the action
 * refuses an argument, and on SEVEN surfaces it does not even mean the call is
 * inert. Measured on the page, dispatching `{zzz: "x"}` with a baseline window
 * subtracted, these answer `success: true` over a key they do not have AND
 * perform a WRITE:
 *
 *     terminal-page.create-terminal          terminal_create   ← SPAWNS A PTY
 *     terminal-page.create-plain-terminal    terminal_create   ← SPAWNS A PTY
 *     terminal-page.open-terminal-window     open_terminal_window
 *     terminal-page.pop-out-active-terminal  open_terminal_window
 *     terminal-page.close-empty-terminal-windows
 *     terminal-page.list-runner-windows
 *     setup-wizard.complete                  complete_setup
 *
 * The first two are the sharp end: an undeclared key is accepted with a `✓`
 * and a PROCESS STARTS.
 *
 * `settings-panel.reset` is deliberately NOT in that list. It does invoke —
 * `get_cloud_sync_settings`, `get_session_metadata_sync_settings`,
 * `get_web_integration_status` — but all three are READS, and counting a read
 * as an effect would inflate the residual in the same direction this paragraph
 * exists to correct. An earlier revision of this comment said EIGHT and
 * included it; that was wrong.
 *
 * Accepted the key with no observed write: `list-layouts`, `list-profiles`,
 * `list-terminals`, `list-tabs`, and the SCC fixture's local-state actions.
 *
 * UNKNOWN rather than clean, and not to be read as either:
 *   - `settings-panel.save` — answered ok with no effect, but only because
 *     nothing was dirty in the harness. It is untested against a dirty form,
 *     so it is not evidence that `save` is inert on an undeclared key.
 *   - `dev-giant-scc-fixture.close` — its only candidate invoke also appeared
 *     in the baseline window, so the measurement cannot separate them.
 *   - Every component on a route the harness never visited (projects,
 *     productivity). Not measured at all.
 *
 * So this list is a FLOOR, not a census.
 *
 * `guardedAction.ts` argues that a `paramSchema: {}` must refuse every
 * supplied key "if it is to be enforced rather than merely documented", and by
 * that standard these 51 surfaces are documented only.
 *
 * This is still a narrower failure than the one this module closes — nothing
 * the caller SENT reached the effect; the effect is the action's own,
 * unconditional. But "no bag can reach it" must not be read as "the call was
 * inert", because for `create-terminal` it is not. It is stated rather than
 * closed because closing it changes the answer 51 wire surfaces give to an
 * argument they currently tolerate, which is a contract change for every agent
 * already calling them and wants its own commit and its own on-page pass.
 * Rule 1 stays as it is; this paragraph exists so the rule is not read as
 * claiming more than it checks.
 *
 * ## The one exemption
 *
 * `guardedAction.ts` itself builds `{id, …, handler}` and is therefore a
 * violation of the rule it enforces. That exemption is a single path, asserted
 * to be a single path by the enforcement test, and paid for there by runtime
 * assertions on the module's actual refusal behaviour — the exempt file is
 * the ONLY file in the tree whose guarding is proven by execution rather than
 * by shape.
 *
 * ## Not app code
 *
 * Node-only (`fs`, `typescript`), imported by tests and by
 * `scripts/`-style tooling. Nothing in the bundle imports it; `typescript` is
 * already a devDependency and `createSourceFile` needs no Program, no tsconfig
 * resolution and no type checker — the same reasoning `keyClaimScan.ts` gives.
 */

import { readdirSync, readFileSync, statSync } from "fs";
import { join, relative, sep } from "path";
import * as ts from "typescript";

/** The wrappers that make a surface guarded by construction. */
export const GUARD_BUILDERS = ["guardedAction", "guardedCustomAction"] as const;

/**
 * The single file exempt from the shape rule, because it IMPLEMENTS the shape
 * rule. Kept as a list of one so the enforcement test can assert the count
 * rather than the membership — a second entry has to be argued for.
 */
export const SHAPE_RULE_EXEMPT = ["src/lib/ui-bridge/guardedAction.ts"] as const;

/** The binders that make a call site of a registry handler a funnel. */
export const BINDERS = ["bindCommand", "bindDirect", "bindSchemaBag"] as const;

/** A test or test-support file: never bundled, so never a wire surface. */
export function isFixtureFile(rel: string): boolean {
  return /\.(test|spec)\.[cm]?[jt]sx?$|\.testkit\.[cm]?[jt]sx?$/.test(rel);
}

/** How a surface was written. */
export type SurfaceForm =
  /** `guardedAction({…})` / `guardedCustomAction({…})` — binding is structural. */
  | "guarded"
  /** An object literal with an `id` and a `handler`. */
  | "literal"
  /**
   * A `CommandAction` in the slash-command registry — an object literal
   * carrying a `slash` as well as an `id` and a `handler`.
   *
   * Guarded by a DIFFERENT mechanism, and a real one: no caller reaches a
   * registry handler except through `bindCommand` (the CommandBar) or
   * `bindDirect` (`runRegistryAction`, and `callRegistry` above it), both of
   * which refuse the bag before the handler exists to receive it. That is the
   * arrangement `bind.ts` was written to establish, and it is asserted rather
   * than assumed: {@link registryHandlerCallSites} enumerates every
   * `x.handler(…)` call in the tree and requires each to bind first or to be a
   * pure forwarder of its own parameters.
   */
  | "registry"
  /**
   * A surface inside a `*.test.*` / `*.testkit.*` file.
   *
   * Counted, printed in the inventory, and NOT judged as a wire surface —
   * Vite bundles from `index.html`'s import graph, which no test module is in,
   * so a fixture action is unreachable from the wire. That claim is asserted
   * (`no app module imports a test module`), not asserted-by-omission: the
   * chord scanner's habit of dropping `*.test.ts` from its walk is exactly how
   * a class becomes invisible.
   */
  | "fixture"
  /**
   * An action-position entry that is a call or a spread: the surface itself is
   * built somewhere else in the tree, where the shape pass sees it.
   */
  | "delegated"
  /**
   * A bare string in an `actions: [...]` array — one of the SDK's built-in
   * element verbs (`"click"`, `"focus"`, …), not a handler at all. Counted so
   * the inventory's total matches what the AST actually contains.
   */
  | "builtin";

export interface ActionSurface {
  /** Repo-relative, forward-slashed. */
  file: string;
  line: number;
  /** Where the AST found it. */
  pass: "position" | "shape" | "guarded";
  /** `actions: [...]`, `customActions: {...}`, or neither (shape pass). */
  position: "component" | "element" | "free";
  form: SurfaceForm;
  /** The action id when it is a string literal; the callee name for a delegate. */
  id: string | null;
  /** Declared parameter count of an inline `handler`; `null` when not inline. */
  handlerArity: number | null;
  /** Whether the surface declares a `paramSchema`. */
  hasParamSchema: boolean;
  /** Non-null when this surface can be reached with an unvalidated bag. */
  violation: string | null;
}

/** `true` for the one filesystem error a concurrent writer can legitimately cause. */
function isVanished(err: unknown): boolean {
  return (err as NodeJS.ErrnoException | undefined)?.code === "ENOENT";
}

/**
 * Every file Vite could bundle out of `src/`.
 *
 * `.jsx`, `.js`, `.mjs` and `.cjs` are here as well as `.tsx?` because Vite
 * bundles all of them. The sibling chord scanner's walk was `/\.tsx?$/`, which
 * made four whole file classes invisible to it — a scanner that reports zero
 * for a class it never looked at.
 *
 * ENOENT is SKIPPED rather than thrown, and only ENOENT. Sibling suites write
 * throwaway probe directories into `src/` and delete them
 * (`globalChords.enforcement.test.ts`'s `__chord_enforcement_probe__` is one),
 * so a directory really can vanish between `readdirSync` and `statSync` when
 * vitest runs files in parallel workers. A file that no longer exists is not
 * in the bundle, so skipping it removes no coverage — but every OTHER error
 * (a permission fault, a bad symlink) still throws, because those are cases
 * where the walk saw LESS than the tree holds and must not pretend otherwise.
 */
export function sourceFiles(dir: string): string[] {
  const out: string[] = [];
  let entries: string[];
  try {
    entries = readdirSync(dir);
  } catch (err) {
    if (isVanished(err)) return out;
    throw err;
  }
  for (const entry of entries) {
    const full = join(dir, entry);
    let isDir: boolean;
    try {
      isDir = statSync(full).isDirectory();
    } catch (err) {
      if (isVanished(err)) continue;
      throw err;
    }
    if (isDir) {
      out.push(...sourceFiles(full));
    } else if (/\.(tsx?|jsx?|mjs|cjs)$/.test(entry)) {
      out.push(full);
    }
  }
  return out;
}

function posix(p: string): string {
  return p.split(sep).join("/");
}

function scriptKind(file: string): ts.ScriptKind {
  if (file.endsWith(".tsx")) return ts.ScriptKind.TSX;
  if (file.endsWith(".jsx")) return ts.ScriptKind.JSX;
  if (/\.(js|mjs|cjs)$/.test(file)) return ts.ScriptKind.JS;
  return ts.ScriptKind.TS;
}

/** Property name as written, for the identifier and string-literal spellings. */
function propName(p: ts.ObjectLiteralElementLike): string | null {
  const n = p.name;
  if (!n) return null;
  if (ts.isIdentifier(n)) return n.text;
  if (ts.isStringLiteral(n)) return n.text;
  return null;
}

/** The name of the function a call expression targets, however it is reached. */
function calleeName(expr: ts.Expression): string | null {
  if (ts.isCallExpression(expr)) {
    const c = expr.expression;
    if (ts.isIdentifier(c)) return c.text;
    if (ts.isPropertyAccessExpression(c)) return c.name.text;
    return null;
  }
  return null;
}

interface LiteralFacts {
  id: string | null;
  /**
   * Whether an `id` property is PRESENT, whatever its initializer.
   *
   * Distinct from {@link LiteralFacts.id}, which is the value only when it is
   * a string literal. Keying the shape pass on the value would let
   * `{ id: ACTION_ID, handler: (params) => … }` — a constant, the most natural
   * way to share an id between a registration and a test — escape the pass
   * entirely.
   */
  hasId: boolean;
  hasHandler: boolean;
  /** `undefined` when there is no handler; `null` when it is not inline. */
  handlerArity: number | null | undefined;
  hasParamSchema: boolean;
  /** `slash` is what distinguishes a registry `CommandAction` from a surface. */
  hasSlash: boolean;
}

function readLiteral(obj: ts.ObjectLiteralExpression): LiteralFacts {
  let id: string | null = null;
  let hasId = false;
  let hasHandler = false;
  let handlerArity: number | null | undefined = undefined;
  let hasParamSchema = false;
  let hasSlash = false;
  for (const p of obj.properties) {
    const name = propName(p);
    if (name === null) continue;
    if (name === "paramSchema") hasParamSchema = true;
    if (name === "slash") hasSlash = true;
    if (name === "id") {
      hasId = true;
      if (ts.isPropertyAssignment(p) && ts.isStringLiteral(p.initializer)) id = p.initializer.text;
    }
    if (name !== "handler") continue;
    hasHandler = true;
    if (ts.isMethodDeclaration(p)) {
      handlerArity = p.parameters.length;
    } else if (ts.isPropertyAssignment(p)) {
      const init = p.initializer;
      handlerArity =
        ts.isArrowFunction(init) || ts.isFunctionExpression(init) ? init.parameters.length : null;
    } else if (ts.isShorthandPropertyAssignment(p)) {
      // `{ handler }` — the function is elsewhere; not decidable here.
      handlerArity = null;
    }
  }
  return { id, hasId, hasHandler, handlerArity, hasParamSchema, hasSlash };
}

const NO_LITERAL: LiteralFacts = {
  id: null,
  hasId: false,
  hasHandler: false,
  handlerArity: undefined,
  hasParamSchema: false,
  hasSlash: false,
};

/**
 * The verdict for one object-literal surface. `null` means it cannot read a
 * bag; a string is the sentence the enforcement test prints.
 */
function verdictForLiteral(facts: LiteralFacts): string | null {
  if (facts.handlerArity === undefined) return null;
  if (facts.handlerArity === null) {
    return "handler is not an inline function — its arity cannot be decided, so it must be written as guardedAction({ … run })";
  }
  if (facts.handlerArity >= 1) {
    return `handler declares ${facts.handlerArity} parameter(s), so it reads the caller's bag itself — write it as guardedAction({ … paramSchema, run })`;
  }
  return null;
}

/**
 * Is this `actions: [...]` a list of ComponentActions, or the SDK's other
 * `actions` — the `StandardAction[]` verb list on an element descriptor
 * (`actions: ["click", "focus"]`)?
 *
 * Two different things wear one property name in the SDK's types, so the
 * discriminator is the CONTENT: a verb list holds strings (and spreads of
 * string arrays), a ComponentAction list holds objects or guard-builder calls.
 * Reading a verb list as a handler list is how `background-observer-service`'s
 * `[...el.actions, ...Object.keys(el.customActions)]` came back as two
 * unresolvable "action factories".
 */
function isVerbList(arr: ts.ArrayLiteralExpression): boolean {
  let sawString = false;
  for (const el of arr.elements) {
    if (ts.isStringLiteralLike(el)) {
      sawString = true;
      continue;
    }
    if (ts.isSpreadElement(el)) continue;
    // An object literal or a call — this is a ComponentAction list.
    return false;
  }
  return sawString || arr.elements.length > 0;
}

/** Scan one source file. `file` is used only for reporting. */
export function scanActionSurfaces(text: string, file: string): ActionSurface[] {
  const rel = posix(file);
  const exempt = (SHAPE_RULE_EXEMPT as readonly string[]).some((e) => rel.endsWith(e));
  const fixture = isFixtureFile(rel);
  const sf = ts.createSourceFile(file, text, ts.ScriptTarget.Latest, true, scriptKind(file));
  const out: ActionSurface[] = [];
  // One entry per AST node, so a literal found by BOTH the position and the
  // shape pass is reported once — with the position pass winning, because
  // where a surface is registered is the more useful thing to print.
  const seen = new Set<number>();

  const at = (node: ts.Node) => sf.getLineAndCharacterOfPosition(node.getStart(sf)).line + 1;

  const pushLiteral = (
    obj: ts.ObjectLiteralExpression,
    pass: ActionSurface["pass"],
    position: ActionSurface["position"],
  ) => {
    if (seen.has(obj.pos)) return;
    seen.add(obj.pos);
    const facts = readLiteral(obj);
    // `registry` and `fixture` are not judged by the shape rule — each has its
    // own gate, asserted elsewhere in the enforcement suite rather than waived
    // here. `exempt` is the single implementation exemption.
    const form: SurfaceForm = fixture ? "fixture" : facts.hasSlash ? "registry" : "literal";
    out.push({
      file: rel,
      line: at(obj),
      pass,
      position,
      form,
      id: facts.id,
      handlerArity: facts.handlerArity ?? null,
      hasParamSchema: facts.hasParamSchema,
      violation: exempt || form !== "literal" ? null : verdictForLiteral(facts),
    });
  };

  const pushEntry = (expr: ts.Expression, position: "component" | "element") => {
    if (ts.isObjectLiteralExpression(expr)) {
      // A `guardedAction({…})` argument object reached through the position
      // pass is not itself a surface — the CALL is. Only bare literals here.
      pushLiteral(expr, "position", position);
      return;
    }
    if (ts.isStringLiteralLike(expr)) {
      out.push({
        file: rel,
        line: at(expr),
        pass: "position",
        position,
        form: "builtin",
        id: expr.text,
        handlerArity: null,
        hasParamSchema: false,
        violation: null,
      });
      return;
    }
    const callee = calleeName(expr);
    if (callee && (GUARD_BUILDERS as readonly string[]).includes(callee)) return; // counted by the guarded pass
    out.push({
      file: rel,
      line: at(expr),
      pass: "position",
      position,
      form: "delegated",
      id: callee,
      handlerArity: null,
      hasParamSchema: false,
      // Not a violation on its own: the object it builds is a `literal` or a
      // `guarded` surface SOMEWHERE in this tree, and the shape pass is
      // tree-wide, so it is judged there. What would escape is a delegate
      // built OUTSIDE `src/` — which `assertDelegatesAreLocal` catches.
      violation: null,
    });
  };

  const visit = (node: ts.Node) => {
    // ── Pass 1: position ────────────────────────────────────────────────
    if (ts.isPropertyAssignment(node)) {
      const name = propName(node);
      if (name === "actions" && ts.isArrayLiteralExpression(node.initializer)) {
        if (isVerbList(node.initializer)) {
          for (const el of node.initializer.elements) {
            if (!ts.isStringLiteralLike(el)) continue;
            out.push({
              file: rel,
              line: at(el),
              pass: "position",
              position: "element",
              form: "builtin",
              id: el.text,
              handlerArity: null,
              hasParamSchema: false,
              violation: null,
            });
          }
          ts.forEachChild(node, visit);
          return;
        }
        for (const el of node.initializer.elements) {
          if (ts.isSpreadElement(el)) {
            out.push({
              file: rel,
              line: at(el),
              pass: "position",
              position: "component",
              form: "delegated",
              id: calleeName(el.expression),
              handlerArity: null,
              hasParamSchema: false,
              violation: null,
            });
            continue;
          }
          pushEntry(el, "component");
        }
      } else if (name === "customActions" && ts.isObjectLiteralExpression(node.initializer)) {
        for (const p of node.initializer.properties) {
          if (ts.isSpreadAssignment(p)) {
            out.push({
              file: rel,
              line: at(p),
              pass: "position",
              position: "element",
              form: "delegated",
              id: calleeName(p.expression),
              handlerArity: null,
              hasParamSchema: false,
              violation: null,
            });
            continue;
          }
          if (ts.isPropertyAssignment(p)) pushEntry(p.initializer, "element");
        }
      } else if (name === "customActions" && ts.isCallExpression(node.initializer)) {
        // `customActions: buildTerminalPaneCustomActions(…)` — the whole map
        // comes from one factory. Same reasoning as a spread.
        out.push({
          file: rel,
          line: at(node.initializer),
          pass: "position",
          position: "element",
          form: "delegated",
          id: calleeName(node.initializer),
          handlerArity: null,
          hasParamSchema: false,
          violation: null,
        });
      }
    }

    // ── Pass 3: guarded ─────────────────────────────────────────────────
    if (ts.isCallExpression(node)) {
      const callee = calleeName(node);
      if (callee && (GUARD_BUILDERS as readonly string[]).includes(callee)) {
        const arg = node.arguments[0];
        const facts = arg && ts.isObjectLiteralExpression(arg) ? readLiteral(arg) : NO_LITERAL;
        if (arg && ts.isObjectLiteralExpression(arg)) seen.add(arg.pos);
        out.push({
          file: rel,
          line: at(node),
          pass: "guarded",
          position: callee === "guardedCustomAction" ? "element" : "component",
          form: "guarded",
          id: facts.id,
          handlerArity: null,
          hasParamSchema: facts.hasParamSchema,
          // A guarded surface that also writes a raw `handler` would be
          // opting back out; the wrapper's type forbids it, and this says so
          // even where the type is bypassed by a cast.
          violation: facts.hasHandler
            ? "a guarded action must not also declare a raw `handler`"
            : null,
        });
      }
    }

    ts.forEachChild(node, visit);
  };
  visit(sf);

  // ── Pass 2: shape (tree-wide, run last so `seen` already holds the
  // literals the other passes claimed) ─────────────────────────────────
  const shapeVisit = (node: ts.Node) => {
    if (ts.isObjectLiteralExpression(node) && !seen.has(node.pos)) {
      const facts = readLiteral(node);
      if (facts.hasHandler && facts.hasId) pushLiteral(node, "shape", "free");
    }
    ts.forEachChild(node, shapeVisit);
  };
  shapeVisit(sf);

  return out.sort((a, b) => a.line - b.line);
}

/**
 * How many `handler` properties this file writes in VALUE position — i.e. in
 * an object literal, as opposed to in an `interface` / type literal, or inside
 * a string.
 *
 * The reconciliation half of the coverage check. A naive text sweep for
 * `handler:` near `actions:` finds four kinds of file the AST correctly
 * reports zero surfaces in: a `ComponentActionDef` interface, a test's
 * `Record<string, {handler}>` cast, this scanner's own fixture SOURCE
 * STRINGS, and `guardedAction.ts`'s two structural interfaces. Rather than
 * tolerate the mismatch — "the sweep over-reports, ignore it" is how a real
 * miss gets ignored too — the check requires each mismatch to have ZERO value
 * handlers, which is a statement about the file rather than about the sweep.
 */
export function valueHandlerProperties(text: string, file: string): number {
  const sf = ts.createSourceFile(file, text, ts.ScriptTarget.Latest, true, scriptKind(file));
  let count = 0;
  const visit = (node: ts.Node) => {
    if (
      ts.isObjectLiteralExpression(node) &&
      node.properties.some((p) => propName(p) === "handler")
    ) {
      count++;
    }
    ts.forEachChild(node, visit);
  };
  visit(sf);
  return count;
}

/** Read a walked file, or `null` if a concurrent writer removed it first. */
function readSourceOrSkip(file: string): string | null {
  try {
    return readFileSync(file, "utf8");
  } catch (err) {
    if (isVanished(err)) return null;
    throw err;
  }
}

/** Scan a whole `src/` tree. `root` is the repo root. */
export function scanTree(root: string): ActionSurface[] {
  const src = join(root, "src");
  const out: ActionSurface[] = [];
  for (const file of sourceFiles(src)) {
    const text = readSourceOrSkip(file);
    if (text === null) continue;
    // Cheap pre-filter: a file with none of these tokens cannot hold a surface.
    if (!/\bhandler\b|\bcustomActions\b|guardedAction|guardedCustomAction/.test(text)) continue;
    out.push(...scanActionSurfaces(text, posix(relative(root, file))));
  }
  return out.sort((a, b) => (a.file === b.file ? a.line - b.line : a.file < b.file ? -1 : 1));
}

/**
 * Delegates whose builder is not declared anywhere in `src/`.
 *
 * A delegate is judged where its object literal is written, and the shape pass
 * only walks `src/`. A factory imported from `node_modules` would therefore be
 * an action surface no pass ever reads — the exact "somewhere the fix did not
 * reach" this module exists to make impossible. Fails closed: an unresolvable
 * name is reported.
 */
export function unresolvedDelegates(surfaces: ActionSurface[], root: string): ActionSurface[] {
  const src = join(root, "src");
  const declared = new Set<string>();
  for (const file of sourceFiles(src)) {
    const text = readSourceOrSkip(file);
    if (text === null) continue;
    for (const m of text.matchAll(
      /(?:export\s+)?(?:async\s+)?function\s+([A-Za-z_$][\w$]*)|(?:const|let)\s+([A-Za-z_$][\w$]*)\s*(?::[^=]+)?=\s*(?:async\s*)?(?:\(|function)/g,
    )) {
      declared.add(m[1] ?? m[2]);
    }
  }
  return surfaces.filter((s) => s.form === "delegated" && (!s.id || !declared.has(s.id)));
}

/** Every surface that can be reached with an unvalidated bag. */
export function violations(surfaces: ActionSurface[]): ActionSurface[] {
  return surfaces.filter((s) => s.violation !== null);
}

/** One `x.handler(…)` call found in app code, and whether a binder precedes it. */
export interface HandlerCallSite {
  file: string;
  line: number;
  /** The text of the call, trimmed — enough to recognise it in a report. */
  text: string;
  /** Why it is safe: `"binds"`, `"forwards"`, or `null` when it is neither. */
  funnel: "binds" | "forwards" | null;
}

/**
 * Every invocation of a registry action's handler in app code, classified.
 *
 * `registry` surfaces are exempt from the shape rule ONLY because their one
 * class of caller binds first. That is a claim about call sites, so it is
 * checked at the call sites rather than trusted:
 *
 *   - **binds** — the enclosing function also calls `bindCommand` /
 *     `bindDirect` / `bindSchemaBag`. `CommandBar`'s dispatch and
 *     `uibridge.ts::runRegistryAction` are both this.
 *   - **forwards** — the call passes exactly the enclosing function's own
 *     parameters, in order, and adds nothing. A pure re-wrap
 *     (`useCommandAction`'s `actionRef.current.handler(args, ctx)`) cannot
 *     introduce an unbound bag, because it has no bag of its own to introduce.
 *
 * Anything else is reported. Test and testkit files are excluded — they are
 * not in the bundle's import graph, which the enforcement suite asserts
 * separately rather than assuming.
 */
export function registryHandlerCallSites(root: string): HandlerCallSite[] {
  const src = join(root, "src");
  const out: HandlerCallSite[] = [];
  for (const abs of sourceFiles(src)) {
    const rel = posix(relative(root, abs));
    if (isFixtureFile(rel)) continue;
    const text = readSourceOrSkip(abs);
    if (text === null) continue;
    if (!/\.handler\s*\(/.test(text)) continue;
    const sf = ts.createSourceFile(abs, text, ts.ScriptTarget.Latest, true, scriptKind(abs));
    const visit = (node: ts.Node) => {
      if (
        ts.isCallExpression(node) &&
        ts.isPropertyAccessExpression(node.expression) &&
        node.expression.name.text === "handler"
      ) {
        out.push({
          file: rel,
          line: sf.getLineAndCharacterOfPosition(node.getStart(sf)).line + 1,
          text: node.getText(sf).replace(/\s+/g, " ").slice(0, 90),
          funnel: classifyCallSite(node, sf),
        });
      }
      ts.forEachChild(node, visit);
    };
    visit(sf);
  }
  return out;
}

function enclosingFunction(node: ts.Node): ts.SignatureDeclaration | null {
  let cur: ts.Node | undefined = node.parent;
  while (cur) {
    if (
      ts.isArrowFunction(cur) ||
      ts.isFunctionExpression(cur) ||
      ts.isFunctionDeclaration(cur) ||
      ts.isMethodDeclaration(cur)
    ) {
      return cur;
    }
    cur = cur.parent;
  }
  return null;
}

function classifyCallSite(call: ts.CallExpression, sf: ts.SourceFile): HandlerCallSite["funnel"] {
  const fn = enclosingFunction(call);
  if (fn) {
    // `forwards`: every argument is one of this function's own parameters, in
    // order, and there are no extras.
    const params = fn.parameters
      .map((p) => (ts.isIdentifier(p.name) ? p.name.text : null))
      .filter((n): n is string => n !== null);
    const args = call.arguments.map((a) => (ts.isIdentifier(a) ? a.text : null));
    if (
      args.length > 0 &&
      args.every((a): a is string => a !== null) &&
      args.every((a, i) => params[i] === a)
    ) {
      return "forwards";
    }
  }
  // `binds`: search outward for a scope that also calls a binder. Outward
  // rather than only the innermost function, because the bind and the call
  // routinely sit in the same `useCallback` body with an inner `try` block
  // between them.
  let scope: ts.Node | null = fn;
  while (scope) {
    let found = false;
    const look = (n: ts.Node) => {
      if (found) return;
      if (ts.isCallExpression(n)) {
        const c = n.expression;
        const name = ts.isIdentifier(c)
          ? c.text
          : ts.isPropertyAccessExpression(c)
            ? c.name.text
            : null;
        if (name && (BINDERS as readonly string[]).includes(name)) found = true;
      }
      ts.forEachChild(n, look);
    };
    look(scope);
    if (found) return "binds";
    scope = enclosingFunction(scope);
  }
  // Neither. Fails CLOSED — an unclassifiable call site is reported, not
  // waived, which is the opposite of how `ESCAPING_CLASS_COUNT` behaved.
  void sf;
  return null;
}

/** A stable, reviewable rendering of the whole inventory. */
export function renderInventory(surfaces: ActionSurface[]): string {
  const lines: string[] = [];
  const counts = new Map<SurfaceForm, number>();
  for (const s of surfaces) counts.set(s.form, (counts.get(s.form) ?? 0) + 1);
  lines.push("# UI Bridge action-surface inventory");
  lines.push("#");
  lines.push("# Generated by src/lib/ui-bridge/actionSurfaces.ts. Regenerate with");
  lines.push("#   pnpm exec vitest run src/lib/ui-bridge/actionSurfaces.enforcement.test.ts");
  lines.push("# after setting UPDATE_ACTION_SURFACE_GOLDEN=1.");
  lines.push("#");
  lines.push(`# total: ${surfaces.length}`);
  for (const form of [
    "guarded",
    "literal",
    "registry",
    "delegated",
    "builtin",
    "fixture",
  ] as SurfaceForm[]) {
    lines.push(`#   ${form}: ${counts.get(form) ?? 0}`);
  }
  lines.push("");
  for (const s of surfaces) {
    lines.push(
      `${s.file}:${s.line}\t${s.form}\t${s.position}\t${s.id ?? "-"}\t` +
        `schema=${s.hasParamSchema ? "yes" : "no"}\tarity=${s.handlerArity ?? "-"}`,
    );
  }
  return lines.join("\n") + "\n";
}
