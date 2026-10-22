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

import {
  assertComponentActionEffectConsistency,
  componentSignatureId,
  normalizeDeclaredComponentSignature,
  type ActionParams,
  type EffectSignature,
  type SemanticSnapshot,
} from "@qontinui/ui-bridge";

import {
  buildCreatePlainTerminalAction,
  CREATE_PLAIN_TERMINAL_ACTION_ID,
} from "@/components/terminal/createPlainTerminalAction";
import { buildTerminalLaunchMenuActions } from "@/components/terminal/terminalLaunchMenuActions";
import * as pageSignatures from "@/components/terminal/terminalPageSignatures";
import type { ZoneViewFacts } from "@/components/terminal/terminalPageSignatures";
import * as pickerSignatures from "@/components/terminal/zonePickerSignatures";

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
  /** The `signature:` initializer as written, or `undefined` when none is declared. */
  signature: ts.Expression | undefined;
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
 * An action element is normally an object literal written inline. Some sites
 * build one with a factory (`buildCreatePlainTerminalAction`), and one builds
 * a whole `actions` LIST with a factory (`buildTerminalLaunchMenuActions`, so
 * its handlers can be tested under the node environment), so the walk FOLLOWS
 * the call rather than skipping it — a skipped element is an unmeasured
 * action, which is the hole this test exists to close.
 *
 * `want` picks the return shape: `"object"` for a single-action factory,
 * `"array"` for a list factory.
 */
function resolveFactoryReturn(
  call: ts.CallExpression,
  corpus: ts.SourceFile[],
): ts.ObjectLiteralExpression | null {
  return resolveFactoryReturnOf(call, corpus, "object") as ts.ObjectLiteralExpression | null;
}

function resolveListFactoryReturn(
  call: ts.CallExpression,
  corpus: ts.SourceFile[],
): ts.ArrayLiteralExpression | null {
  return resolveFactoryReturnOf(call, corpus, "array") as ts.ArrayLiteralExpression | null;
}

function resolveFactoryReturnOf(
  call: ts.CallExpression,
  corpus: ts.SourceFile[],
  want: "object" | "array",
): ts.ObjectLiteralExpression | ts.ArrayLiteralExpression | null {
  const matches = (e: ts.Expression) =>
    want === "object" ? ts.isObjectLiteralExpression(e) : ts.isArrayLiteralExpression(e);
  if (!ts.isIdentifier(call.expression)) return null;
  const name = call.expression.text;

  for (const sf of corpus) {
    let found: ts.ObjectLiteralExpression | ts.ArrayLiteralExpression | null = null;
    const visit = (node: ts.Node): void => {
      if (found) return;
      if (ts.isFunctionDeclaration(node) && node.name?.text === name && node.body) {
        const walkBody = (n: ts.Node): void => {
          if (found) return;
          if (ts.isReturnStatement(n) && n.expression) {
            const expr = ts.isParenthesizedExpression(n.expression)
              ? n.expression.expression
              : n.expression;
            if (matches(expr))
              found = expr as ts.ObjectLiteralExpression | ts.ArrayLiteralExpression;
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

        let actionsNode = objectProperty(arg, "actions");
        if (actionsNode && ts.isCallExpression(actionsNode)) {
          // `actions: buildX(…)` — follow it into the list the factory
          // returns. An unresolvable one is reported as an unreadable element
          // below, never dropped.
          const list = resolveListFactoryReturn(actionsNode, corpus);
          if (!list) {
            actions.push({
              component: componentId,
              action: `<unreadable actions factory: ${actionsNode.getText().slice(0, 60)}>`,
              effect: undefined,
              signature: undefined,
              where,
            });
          }
          actionsNode = list ?? undefined;
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
                signature: undefined,
                where,
              });
              continue;
            }

            actions.push({
              component: componentId,
              action: literalText(objectProperty(literal, "id")) ?? "<unreadable id>",
              effect: literalText(objectProperty(literal, "effect")) ?? undefined,
              signature: objectProperty(literal, "signature"),
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

  it("the list factory's actions are walked, not skipped", () => {
    // `terminal-launch-menu` registers `actions: buildTerminalLaunchMenuActions(…)`.
    // Pin that the walk reached all four, so a factory the resolver stops
    // following reds here rather than silently shrinking the corpus.
    const launch = actions.filter((a) => a.component === "terminal-launch-menu");
    expect(launch.map((a) => a.action).sort()).toEqual([
      "create-ai-session",
      "create-best-account",
      "create-plain",
      "create-with-command",
    ]);
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

// ---------------------------------------------------------------------------
// Effect-calculus signatures on the terminal page's four components
// ---------------------------------------------------------------------------
//
// Plan `2026-10-22-runner-ui-bridge-makes-an-agent-guess-its-own-vocabulary`,
// Phase 2. `predict` on an action with no `signature` answers UNCLASSIFIED, so
// every action on these four components must declare one. The set is read from
// the walk, never pinned as a count: a sibling PR adding an action here must
// add its signature or go red.
//
// The signature each action carries is resolved FROM THE WALK — the
// `signature:` initializer written at that registration — not from a table
// kept beside this test. A signature wired to the wrong action names itself
// after a different action, which is what the id check below catches.

/** The four component ids whose every action must declare a `signature`. */
const SIGNATURE_COMPONENTS = [
  "terminal-page",
  "terminal-launch-menu",
  "zone-layout-picker",
  "zone-profile-picker",
] as const;

/** Non-vacuity floor: the actions those four registered on 2026-10-22. */
const MIN_SIGNATURE_ACTIONS = 22;

/**
 * The walk reports an action id written as an identifier by the identifier's
 * NAME (it is for reporting). The one such id on these components resolves
 * here, so ids compare against the runtime ones.
 */
const ID_CONSTANTS: Record<string, string> = { CREATE_PLAIN_TERMINAL_ACTION_ID };

/** Every export a registration may name in its `signature:` initializer. */
const SIGNATURE_EXPORTS: Record<string, unknown> = { ...pageSignatures, ...pickerSignatures };

/** Zone-view facts with a maximized zone and labelled tiled zones. */
const STUB_ZONE_FACTS: ZoneViewFacts = {
  maximizedZone: 1,
  zoneCount: 4,
  labelledZonesWhenTiled: 2,
};

/**
 * Stand-ins for a factory's NON-literal arguments (render-time values the
 * walk cannot evaluate). A literal argument is always passed as written, so
 * `zoneViewSignature("restore", …)` resolves as the restore signature.
 */
const STUB_ARGS: Record<string, unknown[]> = {
  zoneViewSignature: [undefined, STUB_ZONE_FACTS],
  popOutPageSignature: ["default", 1],
  selectLayoutSignature: [1],
  loadProfileSignature: [{}, 0],
};

/**
 * Evaluate a `signature:` initializer the way the registration does: an
 * identifier names an export; a call invokes the exported factory with its
 * literal arguments and stubs for the rest. Anything else is unresolvable,
 * which the test reports rather than skips.
 */
function resolveSignature(init: ts.Expression): EffectSignature | string {
  if (ts.isIdentifier(init)) {
    const value = SIGNATURE_EXPORTS[init.text];
    return value ? (value as EffectSignature) : `unknown signature export ${init.text}`;
  }
  if (ts.isCallExpression(init) && ts.isIdentifier(init.expression)) {
    const name = init.expression.text;
    const factory = SIGNATURE_EXPORTS[name];
    if (typeof factory !== "function") return `unknown signature factory ${name}`;
    const stubs = STUB_ARGS[name];
    if (!stubs) return `no STUB_ARGS for signature factory ${name}`;
    const args = init.arguments.map((arg, i) =>
      ts.isStringLiteral(arg) || ts.isNoSubstitutionTemplateLiteral(arg) ? arg.text : stubs[i],
    );
    return (factory as (...a: unknown[]) => EffectSignature)(...args);
  }
  return `unresolvable signature initializer ${init.getText()}`;
}

function snapshotOf(ids: string[]): SemanticSnapshot {
  return { elements: ids.map((id) => ({ id })) } as unknown as SemanticSnapshot;
}

/** Pre-snapshots that exercise every branch the conditional `predicts` take. */
const PRE_SNAPSHOTS: SemanticSnapshot[] = [
  snapshotOf([]),
  snapshotOf(["terminal-input-t1", "terminal-input-t2", "terminal-zone-header-0"]),
];

/** Param bags covering the conditional `predicts` (and the empty bag). */
const PARAM_BAGS: Record<string, unknown>[] = [
  {},
  { zoneIndex: 0 },
  { zoneIndex: 1 },
  { layoutId: "single" },
  { terminalId: "t1", windowLabel: "term-1" },
  { terminalId: "t9", windowLabel: "main" },
  { name: "p" },
  { pageId: "other" },
];

describe("component-action signature coverage (terminal page)", () => {
  const { actions } = collect();
  const scoped = actions
    .filter((a) => (SIGNATURE_COMPONENTS as readonly string[]).includes(a.component))
    .map((a) => ({ ...a, action: ID_CONSTANTS[a.action] ?? a.action }));

  /** `<componentId>.<actionId>` → the signature its registration resolves to. */
  const resolved = new Map<string, EffectSignature | string>(
    scoped.map((a) => [
      `${a.component}.${a.action}`,
      a.signature ? resolveSignature(a.signature) : "no `signature` declared",
    ]),
  );
  const signatureOf = (a: { component: string; action: string }): EffectSignature | undefined => {
    const r = resolved.get(`${a.component}.${a.action}`);
    return typeof r === "string" ? undefined : r;
  };

  it("found all four components and at least the 22 actions they registered (non-vacuity floor)", () => {
    expect([...new Set(scoped.map((a) => a.component))].sort()).toEqual(
      [...SIGNATURE_COMPONENTS].sort(),
    );
    expect(scoped.length).toBeGreaterThanOrEqual(MIN_SIGNATURE_ACTIONS);
  });

  it("every action on the four components declares a `signature`", () => {
    const missing = scoped
      .filter((a) => a.signature === undefined)
      .map((a) => `${a.component}.${a.action} (${a.where})`);
    expect(
      missing,
      `${missing.length} action(s) declare no \`signature\`, so \`predict\` answers ` +
        "UNCLASSIFIED for them. Declare one in terminalPageSignatures.ts / " +
        "zonePickerSignatures.ts and add it at the registration.",
    ).toEqual([]);
  });

  it("each registration's signature resolves and names ITS OWN <componentId>.<actionId>", () => {
    const wrong: string[] = [];
    for (const a of scoped) {
      const key = `${a.component}.${a.action}`;
      const r = resolved.get(key);
      if (typeof r === "string" || r === undefined) {
        wrong.push(`${key}: ${r ?? "unresolved"}`);
        continue;
      }
      const normalized = normalizeDeclaredComponentSignature(r, a.component, a.action);
      if (normalized?.id !== componentSignatureId(a.component, a.action)) {
        wrong.push(`${key} carries signature "${normalized?.id}" (${a.where})`);
      }
      if (normalized?.provenance !== "declared") wrong.push(`${key}: provenance not declared`);
    }
    expect(wrong).toEqual([]);
  });

  it("factory-built actions carry their signature at RUNTIME, not only in source", () => {
    const launch = buildTerminalLaunchMenuActions({
      callRegistry: async () => [] as never,
      launchAiSession: async () => [],
    });
    for (const a of launch) {
      expect(a.signature?.id).toBe(componentSignatureId("terminal-launch-menu", a.id));
    }
    const plain = buildCreatePlainTerminalAction(async () => "tab-1");
    expect(plain.signature?.id).toBe(componentSignatureId("terminal-page", plain.id));
  });

  it("every signature agrees with its declared `effect` (the SDK's authoring lint)", () => {
    const messages: string[] = [];
    for (const a of scoped) {
      const sig = signatureOf(a);
      if (!sig) continue;
      for (const pre of PRE_SNAPSHOTS) {
        for (const bag of PARAM_BAGS) {
          const params: ActionParams = { action: a.action, params: bag };
          messages.push(
            ...assertComponentActionEffectConsistency(
              a.effect as "read" | "write" | "destructive" | undefined,
              sig,
              params,
              pre,
              a.component,
              a.action,
            ),
          );
        }
      }
    }
    expect([...new Set(messages)]).toEqual([]);
  });

  it("query actions (list-* / get-*) predict an explicit empty delta, idempotent", () => {
    const queries = scoped.filter((a) => /^(list|get)-/.test(a.action));
    expect(queries.length).toBeGreaterThanOrEqual(5);
    for (const a of queries) {
      const sig = signatureOf(a);
      expect(a.effect).toBe("read");
      expect(sig?.reversibility).toBe("idempotent");
      for (const pre of PRE_SNAPSHOTS) {
        expect(sig?.predicts({ action: a.action, params: {} }, pre)).toEqual({
          errorsAppear: "none",
        });
      }
    }
  });

  it("every destructive action is one-way, including the two the plan names", () => {
    const destructive = scoped.filter((x) => x.effect === "destructive");
    expect(destructive.length).toBeGreaterThanOrEqual(2);
    for (const a of destructive) {
      expect(signatureOf(a)?.reversibility, a.action).toBe("one-way");
    }
    for (const key of [
      "terminal-page.close-empty-terminal-windows",
      "zone-profile-picker.delete-profile",
    ]) {
      const r = resolved.get(key);
      expect(typeof r === "string" ? r : r?.reversibility, key).toBe("one-way");
    }
  });
});
