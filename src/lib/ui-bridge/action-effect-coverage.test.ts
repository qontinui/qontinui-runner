/**
 * Enumerated coverage floor for the component-action `effect` safety class.
 *
 * Plan `2026-09-04-effect-calculus-joins-the-component-action-registry`,
 * Phase 2. Phase 1 proved ONE annotation crosses the SDK -> runner -> Rust
 * boundary; Phase 2 annotated all 60 registered actions. This test is what
 * stops that set decaying: it walks EVERY `useUIComponent({ ... })`
 * registration in `src/` and fails when any action lacks an `effect`.
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
 * WHY THIS IS A STATIC WALK AND NOT A MOUNT. Mounting the 18 registering
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
 * Non-vacuity floors. Measured 2026-09-04: the walk finds 27
 * `useUIComponent` registrations carrying 60 actions between them. Only 18 of
 * the 27 declare an `actions` array at all — the other 9 register elements and
 * page state only, which is why the component floor is set at the
 * action-declaring count rather than at 27.
 *
 * Deliberately `>=`, not `===`. The invariant this file protects is "every
 * action is annotated", and pinning an exact total would red on every
 * legitimately-added component — a test that fails for the right reason at the
 * wrong times gets weakened, and a weakened test protects nothing. The floors
 * catch the failure mode that actually matters here: a walk that silently
 * stops finding registrations.
 */
const MIN_COMPONENTS = 18;
const MIN_ACTIONS = 60;

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

function collect(): { actions: FoundAction[]; components: Set<string> } {
  const files = sourceFiles(SRC_ROOT);
  const corpus = files.map(parse);
  const actions: FoundAction[] = [];
  const components = new Set<string>();

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
        components.add(componentId);

        const actionsNode = objectProperty(arg, "actions");
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
      ts.forEachChild(node, visit);
    };
    visit(sf);
  }

  return { actions, components };
}

describe("component-action effect coverage", () => {
  const { actions, components } = collect();

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

  it("the factory-built action carries its effect at RUNTIME, not only in source", () => {
    // The AST walk above follows `buildCreatePlainTerminalAction` statically.
    // This asserts the built object really has the field, so a change that
    // satisfies the parser without reaching the registration is still caught.
    const action = buildCreatePlainTerminalAction(async () => "tab-1");
    expect(action.effect).toBe("write");
    expect(VALID_EFFECTS.has(action.effect)).toBe(true);
  });
});
