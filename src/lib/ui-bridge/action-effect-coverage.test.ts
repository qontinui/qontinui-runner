/**
 * Enumerated coverage floor for the `effect` safety class, over BOTH surfaces
 * that carry one: component actions and element custom actions.
 *
 * Plan `2026-09-04-effect-calculus-joins-the-component-action-registry`,
 * Phase 2 (component actions) and Phase 4 (element custom actions). Phase 1
 * proved ONE annotation crosses the SDK -> runner -> Rust boundary; Phase 2
 * annotated all registered component actions and Phase 4 annotated the 12
 * element custom actions. This test is what stops either set decaying: it
 * walks EVERY `useUIComponent({ ... })` registration and EVERY literal
 * `customActions: { ... }` registration in `src/`, and fails when any action
 * lacks an `effect`.
 *
 * WHY ELEMENT CUSTOM ACTIONS ARE WALKED HERE TOO. Phase 4 annotated them, but
 * nothing enforced the annotation: the Phase-3 ESLint rule matches only
 * `useUIComponent({ actions })`, and the CI effect fixture captures components
 * only. Measured 2026-09-20 on `origin/main`, deleting `effect: "read"` from
 * `TerminalBridgeProxies`' `focus` entry left `lint`, this test and
 * `effect:fixture:check` ALL green. Eight of the twelve are `destructive` raw
 * PTY writes, so that was the widest unratcheted surface in the calculus.
 * Extending the Phase-3 lint rule to `customActions` object literals is the
 * other half and lives in `ui-bridge`; this floor does not wait on a plugin
 * publish, and an enumerated floor is in any case what the plan's graduation
 * condition (a) actually names — the one in "Out of scope — named, not
 * silently deferred", which gates a destructive-invocation guard, not a phase.
 * It asks for "60/60 component actions and 9/9 element custom actions
 * annotated"; those totals are now 64 and 12.
 *
 * WHY THE INVARIANT MATTERS. An absent `effect` is UNCLASSIFIED, never `read`
 * — the serializer forwards it undefaulted on purpose, and no verb in the
 * SDK's `STANDARD_ACTION_EFFECTS` map can ever yield `destructive`. So an
 * action added without an annotation is indistinguishable, to an autonomous
 * walk, from a safe one [policy: `testing`
 * `an-actions-safety-class-is-declared-not-re-derived` part 2]. The
 * classification rubric an author applies is
 * `src-tauri/src/mcp/ui_bridge/CONTRACT.md`, "The `effect` classification
 * rubric".
 *
 * WHY THIS IS A STATIC WALK AND NOT A MOUNT. Mounting the 27 registering
 * components needs Tauri IPC, a DOM, and most of the app's React context tree;
 * a mount-based walk would cover only the components it managed to mount and
 * report green while measuring a subset — coverage decided by what was
 * convenient rather than by enumeration [policy: `testing`
 * `coverage-is-enumerated-not-salient`]. The AST walk also reaches
 * registrations that no test ever mounts, which is where an unannotated action
 * would actually hide.
 *
 * WHY THE FLOORS BELOW EXIST. A walk that finds nothing passes vacuously — a
 * renamed hook, a changed file layout, or a broken parse would turn this test
 * green while measuring zero [policy: `testing` `a-green-run-must-prove-it-ran`,
 * `silent-empty-is-unknown`]. The component and action floors are the positive
 * evidence that the walk actually ran over the corpus.
 */

import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, resolve } from "node:path";

import ts from "typescript";
import { describe, expect, it } from "vitest";

import { buildCreatePlainTerminalAction } from "@/components/terminal/createPlainTerminalAction";

const SRC_ROOT = resolve(__dirname, "../..");

/** The three values of `IREffect` / `IrEffect`. */
const VALID_EFFECTS = new Set(["read", "write", "destructive"]);

/**
 * Non-vacuity floors. Measured 2026-09-20 on `origin/main`: the walk finds 27
 * `useUIComponent` registrations, 21 of which declare an `actions` array,
 * carrying 64 actions between them; and 2 literal `customActions`
 * registrations carrying 12 entries.
 *
 * `MIN_ELEMENT_SITES` counts FILES carrying at least one literal
 * `customActions` map, not registrations — a second map added inside one of
 * the two existing files does not move it. `MIN_ELEMENT_ACTIONS` is the floor
 * that counts entries.
 *
 * `MIN_COMPONENTS` is counted against the ACTION-DECLARING registrations (21),
 * not against all 27. Until 2026-09-20 the floor was 18 but the quantity
 * compared to it was the full set of 27, so nine registrations could have
 * vanished before it tripped — the comment described the intended measurement
 * and the code measured something looser.
 *
 * Deliberately `>=`, not `===`: adding an action must never red this file, so
 * the floors are one-directional. Because they are, they are set AT the
 * measured corpus rather than below it — slack below the measurement buys
 * nothing and costs the guarantee, since it is exactly the room in which the
 * corpus can shrink silently. Before 2026-09-20 the action floor sat at 60
 * against 64 and the component floor at 18 against a mis-measured 27, so four
 * annotated actions and nine registrations could have disappeared with this
 * file still green while its header claimed the corpus "cannot silently
 * shrink".
 *
 * A DELETION IS THEREFORE EXPECTED TO MOVE A FLOOR, and that is the point:
 * removing an annotated action is a deliberate act that should be visible in
 * the diff and argued for in review, not absorbed by slack. #1463 deleted a
 * registration (`usePageRegistration.ts`) and no floor moved, because the
 * component floor was being compared against the wrong quantity.
 */
const MIN_COMPONENTS = 21;
const MIN_ACTIONS = 64;
const MIN_ELEMENT_SITES = 2;
const MIN_ELEMENT_ACTIONS = 12;

interface FoundAction {
  /** Component id as written (a template literal is rendered with its `${}`). */
  component: string;
  /** Action id as written, or a description of why it could not be read. */
  action: string;
  /** `undefined` when the registration declares no `effect`. */
  effect: string | undefined;
  where: string;
}

function sourceFiles(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir)) {
    if (entry === "node_modules" || entry === "generated") continue;
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) {
      sourceFiles(full, out);
    } else if (/\.tsx?$/.test(entry) && !/\.(test|spec)\.tsx?$/.test(entry)) {
      out.push(full);
    }
  }
  return out;
}

function parse(file: string): ts.SourceFile {
  return ts.createSourceFile(
    file,
    readFileSync(file, "utf8"),
    ts.ScriptTarget.Latest,
    true,
    file.endsWith(".tsx") ? ts.ScriptKind.TSX : ts.ScriptKind.TS,
  );
}

function propertyName(name: ts.PropertyName): string | null {
  if (ts.isIdentifier(name) || ts.isStringLiteral(name)) return name.text;
  return null;
}

function objectProperty(node: ts.ObjectLiteralExpression, key: string): ts.Expression | undefined {
  for (const prop of node.properties) {
    if (ts.isPropertyAssignment(prop) && propertyName(prop.name) === key) {
      return prop.initializer;
    }
  }
  return undefined;
}

/** The literal text of a `string` / template initializer, for reporting only. */
function literalText(node: ts.Expression | undefined): string | null {
  if (!node) return null;
  if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) return node.text;
  if (ts.isTemplateExpression(node)) return node.getText();
  if (ts.isBinaryExpression(node) && node.operatorToken.kind === ts.SyntaxKind.PlusToken) {
    const left = literalText(node.left);
    const right = literalText(node.right);
    return left !== null && right !== null ? left + right : null;
  }
  if (ts.isIdentifier(node)) return node.text;
  return null;
}

/**
 * An action element is normally an object literal written inline. Exactly one
 * site builds it with a factory (`buildCreatePlainTerminalAction`), so the walk
 * FOLLOWS the call rather than skipping it — a skipped element is an
 * unmeasured action, which is the hole this test exists to close.
 */
function resolveFactoryReturn(
  call: ts.CallExpression,
  corpus: ts.SourceFile[],
): ts.ObjectLiteralExpression | null {
  if (!ts.isIdentifier(call.expression)) return null;
  const name = call.expression.text;

  for (const sf of corpus) {
    let found: ts.ObjectLiteralExpression | null = null;
    const visit = (node: ts.Node): void => {
      if (found) return;
      if (ts.isFunctionDeclaration(node) && node.name?.text === name && node.body) {
        const walkBody = (n: ts.Node): void => {
          if (found) return;
          if (ts.isReturnStatement(n) && n.expression) {
            const expr = ts.isParenthesizedExpression(n.expression)
              ? n.expression.expression
              : n.expression;
            if (ts.isObjectLiteralExpression(expr)) found = expr;
          }
          ts.forEachChild(n, walkBody);
        };
        walkBody(node.body);
      }
      ts.forEachChild(node, visit);
    };
    visit(sf);
    if (found) return found;
  }
  return null;
}

interface Collected {
  /** Component actions, from `useUIComponent({ actions: [...] })`. */
  actions: FoundAction[];
  /** Registrations that declare an `actions` property at all. */
  components: Set<string>;
  /** Element custom actions, from a literal `customActions: { ... }` map. */
  elementActions: FoundAction[];
  /** Files carrying at least one literal `customActions` registration. */
  elementSites: Set<string>;
}

/**
 * Resolve an expression to the object literal it denotes, following the same
 * two indirections `resolveFactoryReturn` already follows for a component
 * action: a factory call, and a module-scope `const` binding.
 *
 * This exists so the element walk is not a reason NOT to refactor. The two
 * terminal `customActions` maps overlap on five identically-annotated entries
 * (`sendKeys`, `writeToTerminal`, `paste`, `pasteText`, `getScrollback`), and
 * hoisting them into a shared builder is the obvious next cleanup. Without
 * resolution that cleanup would take the element floors to zero with no way to
 * stay green — the floor penalising the de-duplication rather than a real loss
 * of coverage. The component side already had `resolveFactoryReturn` for
 * exactly this; the element side now shares it.
 */
function resolveObjectLiteral(
  expr: ts.Expression,
  corpus: ts.SourceFile[],
): ts.ObjectLiteralExpression | null {
  if (ts.isObjectLiteralExpression(expr)) return expr;
  if (ts.isCallExpression(expr)) return resolveFactoryReturn(expr, corpus);
  if (ts.isIdentifier(expr)) {
    const name = expr.text;
    for (const sf of corpus) {
      let found: ts.ObjectLiteralExpression | null = null;
      const visit = (node: ts.Node): void => {
        if (found) return;
        if (
          ts.isVariableDeclaration(node) &&
          ts.isIdentifier(node.name) &&
          node.name.text === name &&
          node.initializer &&
          ts.isObjectLiteralExpression(node.initializer)
        ) {
          found = node.initializer;
          return;
        }
        ts.forEachChild(node, visit);
      };
      visit(sf);
      if (found) return found;
    }
  }
  return null;
}

function collect(): Collected {
  const files = sourceFiles(SRC_ROOT);
  const corpus = files.map(parse);
  const actions: FoundAction[] = [];
  const components = new Set<string>();
  const elementActions: FoundAction[] = [];
  const elementSites = new Set<string>();

  for (const sf of corpus) {
    const where = relative(SRC_ROOT, sf.fileName);

    const visit = (node: ts.Node): void => {
      if (
        ts.isCallExpression(node) &&
        ts.isIdentifier(node.expression) &&
        node.expression.text === "useUIComponent" &&
        node.arguments.length >= 1 &&
        ts.isObjectLiteralExpression(node.arguments[0])
      ) {
        const arg = node.arguments[0];
        const componentId = literalText(objectProperty(arg, "id")) ?? `<unreadable in ${where}>`;

        const actionsNode = objectProperty(arg, "actions");
        // The floor counts registrations that DECLARE an `actions` property.
        // The other six carry nothing this file can assert on — five pass only
        // `id` / `name` / `description` and one also passes `state`; any
        // elements they expose come from separate `useUIElement` calls. Adding
        // them would inflate the floor's denominator without adding coverage.
        //
        // Note this counts the PROPERTY, not a non-empty array:
        // `components/app/TabContent.tsx` registers `actions: []` and is one of
        // the 21, so the component floor can be held up by a registration
        // carrying no annotated action. `MIN_ACTIONS` is the floor that counts
        // actions.
        if (actionsNode) components.add(componentId);

        if (actionsNode && !ts.isArrayLiteralExpression(actionsNode)) {
          // `actions` present but not enumerable here — e.g. `actions: props`
          // or `actions: xs || []`. Reported as an UNMEASURED action rather
          // than skipped, for the same reason an unreadable element is below:
          // a walk that passes silently over a registration it cannot read
          // reports coverage it did not measure [policy: `testing`
          // `silent-empty-is-unknown`].
          //
          // This is the exact shape that hid the one unannotated action
          // `require-action-effect` found when Phase 3 turned it on:
          // `usePageRegistration.ts` forwarded `actions: actions || []` and
          // this walk said nothing. The rule caught it; this floor did not.
          // The rule RESOLVES such an expression, which is its job as an
          // author-time ratchet. An enumerating floor does not guess: it
          // reports what it cannot enumerate and makes someone look.
          //
          // CONSEQUENCE, stated so it is not a surprise: hoisting a fully
          // annotated `actions: [...]` out of its call site leaves `lint`
          // GREEN (the rule follows it) and this file RED (the walk will not).
          // The two gates then disagree on a legal corpus. That is deliberate
          // — a floor that guesses is not a floor — and the fix is to inline
          // the array or to teach the walk that shape, never to drop the
          // report.
          actions.push({
            component: componentId,
            action: `<unenumerable actions: ${actionsNode.getText().slice(0, 60)}>`,
            effect: undefined,
            where,
          });
        }

        if (actionsNode && ts.isArrayLiteralExpression(actionsNode)) {
          for (const el of actionsNode.elements) {
            let literal: ts.ObjectLiteralExpression | null = null;
            if (ts.isObjectLiteralExpression(el)) {
              literal = el;
            } else if (ts.isCallExpression(el)) {
              literal = resolveFactoryReturn(el, corpus);
            }

            if (!literal) {
              // Reported as an action with NO effect rather than skipped: an
              // element the walk cannot read is unmeasured, and unmeasured is
              // UNKNOWN, not annotated.
              actions.push({
                component: componentId,
                action: `<unreadable element: ${el.getText().slice(0, 60)}>`,
                effect: undefined,
                where,
              });
              continue;
            }

            actions.push({
              component: componentId,
              action: literalText(objectProperty(literal, "id")) ?? "<unreadable id>",
              effect: literalText(objectProperty(literal, "effect")) ?? undefined,
              where,
            });
          }
        }
      }

      // ── Element custom actions ────────────────────────────────────────
      // A REGISTRATION is a `customActions` property that RESOLVES to an
      // object-literal map of entry objects — written inline, or reached
      // through a factory call or a module-scope binding. The discriminator is
      // structural, never a file or identifier allow-list.
      //
      // The same property name also appears on the PROJECTION side, where the
      // initializer is a call that resolves to nothing local —
      // `background-observer-service.ts` forwards
      // `serializeElementCustomActions(el.customActions)`, whose callee is an
      // SDK import with no declaration in this corpus. That hop cannot lose an
      // annotation it never authored, and it has its own mutation-checked test
      // asserting the `effect` survives it and that an unclassified entry
      // stays unclassified (`src/services/background-observer-service.test.ts`).
      // So an initializer that does not resolve HERE is treated as a
      // projection and is not reported — a registration and a projection are
      // otherwise indistinguishable at this property, and reporting every
      // unresolved one would red on the serializer forever.
      //
      // The site and entry floors below are what stop that scoping being
      // silently wrong: if a real registration ever changes into a shape this
      // cannot resolve, the floors red rather than the walk quietly measuring
      // less. That is the whole reason they are set AT the measurement.
      if (ts.isPropertyAssignment(node) && propertyName(node.name) === "customActions") {
        const map = resolveObjectLiteral(node.initializer, corpus);
        if (map) {
          elementSites.add(where);
          for (const entry of map.properties) {
            const value =
              ts.isPropertyAssignment(entry) && entry.initializer
                ? resolveObjectLiteral(entry.initializer, corpus)
                : null;
            if (!value) {
              elementActions.push({
                component: where,
                action: `<unreadable custom action: ${entry.getText().slice(0, 60)}>`,
                effect: undefined,
                where,
              });
              continue;
            }
            elementActions.push({
              component: propertyName(entry.name) ?? "<unreadable key>",
              action:
                literalText(objectProperty(value, "id")) ??
                propertyName(entry.name) ??
                "<unreadable id>",
              effect: literalText(objectProperty(value, "effect")) ?? undefined,
              where,
            });
          }
        }
      }

      ts.forEachChild(node, visit);
    };
    visit(sf);
  }

  return { actions, components, elementActions, elementSites };
}

describe("action `effect` coverage (component actions and element custom actions)", () => {
  const { actions, components, elementActions, elementSites } = collect();

  it("found the whole registration corpus (non-vacuity floor)", () => {
    expect(components.size).toBeGreaterThanOrEqual(MIN_COMPONENTS);
    expect(actions.length).toBeGreaterThanOrEqual(MIN_ACTIONS);
  });

  it("every registered component action declares an `effect`", () => {
    const unannotated = actions
      .filter((a) => a.effect === undefined)
      .map((a) => `${a.component}.${a.action} (${a.where})`);

    expect(
      unannotated,
      `${unannotated.length} component action(s) carry no \`effect\`. An absent effect is ` +
        "UNCLASSIFIED, not `read` — an autonomous walk cannot tell it from a safe action. " +
        "Classify each against the rubric in src-tauri/src/mcp/ui_bridge/CONTRACT.md " +
        '("The `effect` classification rubric") and annotate it at the call site.',
    ).toEqual([]);
  });

  it("every declared `effect` is one of the three IREffect values", () => {
    const bad = actions
      .filter((a) => a.effect !== undefined && !VALID_EFFECTS.has(a.effect))
      .map((a) => `${a.component}.${a.action} = ${a.effect} (${a.where})`);

    expect(bad).toEqual([]);
  });

  it("found the element custom-action corpus (non-vacuity floor)", () => {
    expect(elementSites.size).toBeGreaterThanOrEqual(MIN_ELEMENT_SITES);
    expect(elementActions.length).toBeGreaterThanOrEqual(MIN_ELEMENT_ACTIONS);
  });

  it("every registered element custom action declares an `effect`", () => {
    const unannotated = elementActions
      .filter((a) => a.effect === undefined)
      .map((a) => `${a.component}.${a.action} (${a.where})`);

    expect(
      unannotated,
      `${unannotated.length} element custom action(s) carry no \`effect\`. Eight of the ` +
        "twelve registered here are `destructive` raw PTY writes, and an absent effect is " +
        "UNCLASSIFIED, not `read`. Classify each against the rubric in " +
        'src-tauri/src/mcp/ui_bridge/CONTRACT.md ("The `effect` classification rubric") ' +
        "and annotate it at the registration.",
    ).toEqual([]);
  });

  it("every declared element custom-action `effect` is one of the three IREffect values", () => {
    const bad = elementActions
      .filter((a) => a.effect !== undefined && !VALID_EFFECTS.has(a.effect))
      .map((a) => `${a.component}.${a.action} = ${a.effect} (${a.where})`);

    expect(bad).toEqual([]);
  });

  it("the factory-built action carries its effect at RUNTIME, not only in source", () => {
    // The AST walk above follows `buildCreatePlainTerminalAction` statically.
    // This asserts the built object really has the field, so a change that
    // satisfies the parser without reaching the registration is still caught.
    const action = buildCreatePlainTerminalAction(async () => "tab-1");
    expect(action.effect).toBe("write");
    expect(VALID_EFFECTS.has(action.effect)).toBe(true);
  });
});
