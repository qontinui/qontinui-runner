#!/usr/bin/env node
// Component-action `effect` boundary fixture capture.
//
// Plan 2026-09-04-effect-calculus-joins-the-component-action-registry, Phase 1.
//
// WHY THIS EXISTS
// ---------------
// The coarse `effect` annotation (`'read' | 'write' | 'destructive'`) is
// declared by an app author on a `ComponentActionDef` in the SDK, and read on
// the Rust side as `qontinui_types::ui_bridge::ComponentActionInfo::effect`.
// Both halves compiled fine for weeks while the value did NOT cross the
// boundary: the runner's `serializeComponent` per-action projection is a CLOSED
// list of explicit picks with no spread, so an unlisted field is silently
// dropped on the way out. The Rust type declaring the field is therefore NOT
// evidence that any value ever reaches it.
//
// This script captures the actual `/control/components` response body the
// runner would emit for the `settings-panel` fixture, by:
//
//   1. parsing the REAL `useUIComponent({...})` registration out of
//      `src/components/settings/Settings.tsx` (TS AST — no hand-typed mirror of
//      the annotations), then
//   2. running the REAL `serializeComponent` from
//      `src/hooks/ui-bridge-events/utils.ts` over it (transpiled in-process, so
//      the allow-list under test is the shipped one), then
//   3. wrapping the result in the envelope
//      `ui_bridge_get_components_handler` emits (`{success, data:{components}}`
//      — see `src-tauri/src/mcp/ui_bridge/elements.rs`,
//      `components_envelope_tests`).
//
// The captured body is committed at
// `src-tauri/tests/fixtures/control-components-effect.json` and deserialized by
// `src-tauri/tests/component_effect_fixture.rs` into
// `Vec<qontinui_types::ui_bridge::UIBridgeComponent>`. So the pair spans the
// whole seam: SDK annotation -> runner serializer -> Rust type.
//
// Deleting the annotation in Settings.tsx, or the `effect:` line from
// `serializeComponent`, turns this script's verify mode red immediately and the
// Rust test red as soon as the fixture is regenerated. That is the point: the
// checks were mutation-tested against exactly those two deletions.
//
// Usage:
//   node scripts/capture-component-effect-fixture.cjs            # verify (CI)
//   node scripts/capture-component-effect-fixture.cjs --update   # regenerate
//
// Exit codes: 0 = fixture matches (or was written), 1 = drift / capture failure.

"use strict";

const fs = require("node:fs");
const path = require("node:path");
const ts = require("typescript");

const REPO_ROOT = path.resolve(__dirname, "..");
const SETTINGS_TSX = path.join(REPO_ROOT, "src/components/settings/Settings.tsx");
const UTILS_TS = path.join(REPO_ROOT, "src/hooks/ui-bridge-events/utils.ts");
const FIXTURE = path.join(REPO_ROOT, "src-tauri/tests/fixtures/control-components-effect.json");

const UPDATE = process.argv.includes("--update");

/** The component whose registration is the probe fixture. */
const FIXTURE_COMPONENT_ID = "settings-panel";

/**
 * The annotations this capture exists to carry, used ONLY to make a drift
 * message diagnostic.
 *
 * These are deliberately NOT asserted here. The layering is: this script
 * CAPTURES faithfully and reports drift; `src-tauri/tests/
 * component_effect_fixture.rs` ASSERTS. Asserting in both places would mean a
 * deleted annotation could never be carried through to the Rust test at all —
 * the capture would refuse to regenerate — and a check whose failure mode
 * cannot be reached is a check nobody can mutation-test.
 */
const EXPECTED_EFFECTS = { "list-tabs": "read", "switch-tab": "write" };

/**
 * Frozen so the capture is deterministic — `registeredAt` is a wall clock on a
 * live runner, and a moving value would make every re-capture look like drift.
 */
const FROZEN_REGISTERED_AT = 1_756_944_000_000; // 2026-09-04T00:00:00Z

function fail(message) {
  console.error(`ERROR: ${message}`);
  process.exit(1);
}

// --- 1. Statically evaluate the pieces of an object literal we can ----------

const UNRESOLVED = Symbol("unresolved");

/**
 * Evaluate the subset of expressions an action declaration legitimately uses:
 * literals, `+`-concatenated string literals (the `list-tabs` description is
 * written that way), object literals and array literals. Anything else — a
 * handler, an identifier, a call — comes back UNRESOLVED and is dropped by the
 * caller rather than guessed at.
 */
function evalStatic(node) {
  if (!node) return UNRESOLVED;
  if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) return node.text;
  if (ts.isNumericLiteral(node)) return Number(node.text);
  if (node.kind === ts.SyntaxKind.TrueKeyword) return true;
  if (node.kind === ts.SyntaxKind.FalseKeyword) return false;
  if (node.kind === ts.SyntaxKind.NullKeyword) return null;
  if (ts.isParenthesizedExpression(node)) return evalStatic(node.expression);
  if (ts.isAsExpression(node) || ts.isSatisfiesExpression(node)) return evalStatic(node.expression);
  if (ts.isBinaryExpression(node) && node.operatorToken.kind === ts.SyntaxKind.PlusToken) {
    const left = evalStatic(node.left);
    const right = evalStatic(node.right);
    if (typeof left === "string" && typeof right === "string") return left + right;
    return UNRESOLVED;
  }
  if (ts.isArrayLiteralExpression(node)) {
    const out = [];
    for (const el of node.elements) {
      const v = evalStatic(el);
      if (v === UNRESOLVED) return UNRESOLVED;
      out.push(v);
    }
    return out;
  }
  if (ts.isObjectLiteralExpression(node)) {
    const out = {};
    for (const prop of node.properties) {
      if (!ts.isPropertyAssignment(prop)) continue;
      const key = propertyName(prop.name);
      if (key === null) continue;
      const v = evalStatic(prop.initializer);
      if (v === UNRESOLVED) continue;
      out[key] = v;
    }
    return out;
  }
  return UNRESOLVED;
}

function propertyName(name) {
  if (ts.isIdentifier(name)) return name.text;
  if (ts.isStringLiteral(name)) return name.text;
  return null;
}

function objectProperty(objectLiteral, key) {
  const prop = objectLiteral.properties.find(
    (p) => ts.isPropertyAssignment(p) && propertyName(p.name) === key,
  );
  return prop ? prop.initializer : null;
}

// --- 2. Pull the fixture component's registration out of Settings.tsx -------

/** Fields of a `ComponentActionDef` that are DATA (a handler is not). */
const ACTION_FIELDS = ["id", "label", "description", "paramSchema", "effect"];

function readFixtureRegistration() {
  const src = fs.readFileSync(SETTINGS_TSX, "utf8");
  const sf = ts.createSourceFile(SETTINGS_TSX, src, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);

  let found = null;
  const visit = (node) => {
    if (
      ts.isCallExpression(node) &&
      ts.isIdentifier(node.expression) &&
      node.expression.text === "useUIComponent" &&
      node.arguments.length >= 1 &&
      ts.isObjectLiteralExpression(node.arguments[0])
    ) {
      const arg = node.arguments[0];
      const id = evalStatic(objectProperty(arg, "id"));
      if (id === FIXTURE_COMPONENT_ID) found = arg;
    }
    ts.forEachChild(node, visit);
  };
  visit(sf);

  if (!found) {
    fail(
      `no \`useUIComponent({ id: "${FIXTURE_COMPONENT_ID}", ... })\` call found in ` +
        `${path.relative(REPO_ROOT, SETTINGS_TSX)}. The effect boundary fixture is gone — ` +
        `re-point FIXTURE_COMPONENT_ID at a component that is still registered, and update ` +
        `scripts/contract-smoke.ps1 Probe 2b + src-tauri/tests/component_effect_fixture.rs to match.`,
    );
  }

  const actionsNode = objectProperty(found, "actions");
  if (!actionsNode || !ts.isArrayLiteralExpression(actionsNode)) {
    fail(`\`${FIXTURE_COMPONENT_ID}\` has no array-literal \`actions\` property to capture.`);
  }

  const actions = [];
  for (const el of actionsNode.elements) {
    if (!ts.isObjectLiteralExpression(el)) {
      fail(
        `\`${FIXTURE_COMPONENT_ID}.actions\` contains a non-object-literal entry ` +
          `(spread/computed?). The capture needs plain literals — update this parser.`,
      );
    }
    const action = {};
    for (const field of ACTION_FIELDS) {
      const v = evalStatic(objectProperty(el, field));
      if (v !== UNRESOLVED && v !== undefined) action[field] = v;
    }
    if (typeof action.id !== "string") {
      fail(`an action of \`${FIXTURE_COMPONENT_ID}\` has no statically-readable string \`id\`.`);
    }
    actions.push(action);
  }

  const component = {
    id: FIXTURE_COMPONENT_ID,
    name: evalStatic(objectProperty(found, "name")),
    description: evalStatic(objectProperty(found, "description")),
    actions,
    // Not declared on the registration; the live registry fills these in.
    // Frozen here so re-capture is deterministic (see FROZEN_REGISTERED_AT).
    elementIds: [],
    registeredAt: FROZEN_REGISTERED_AT,
    mounted: true,
  };
  for (const key of ["name", "description"]) {
    if (component[key] === UNRESOLVED) delete component[key];
  }

  return component;
}

// --- 3. Load the REAL serializeComponent from utils.ts ----------------------

/**
 * `utils.ts` imports two runtime modules that `serializeComponent` never
 * touches. Stubbing them keeps the capture free of a DOM/Tauri environment
 * while leaving the function under test untouched. An unexpected import throws
 * rather than resolving to an empty object — a new runtime dependency in this
 * module is something the capture must surface, not paper over.
 */
const MODULE_STUBS = {
  "@/lib/runner-api": { getApiPort: () => 0 },
  "@qontinui/ui-bridge": { serializeRegisteredElement: () => ({}) },
  "./types": {},
};

function loadSerializeComponent() {
  const source = fs.readFileSync(UTILS_TS, "utf8");
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
      esModuleInterop: true,
    },
    fileName: UTILS_TS,
  });

  const moduleObj = { exports: {} };
  const stubRequire = (specifier) => {
    if (Object.prototype.hasOwnProperty.call(MODULE_STUBS, specifier)) {
      return MODULE_STUBS[specifier];
    }
    throw new Error(
      `capture-component-effect-fixture: unexpected runtime import "${specifier}" in ` +
        `${path.relative(REPO_ROOT, UTILS_TS)}. Add a stub to MODULE_STUBS if ` +
        `serializeComponent does not depend on it.`,
    );
  };

  // eslint-disable-next-line no-new-func -- deliberate: the function under test
  // must be the shipped one, not a re-implementation.
  const factory = new Function("exports", "require", "module", "__filename", "__dirname", outputText);
  factory(moduleObj.exports, stubRequire, moduleObj, UTILS_TS, path.dirname(UTILS_TS));

  const fn = moduleObj.exports.serializeComponent;
  if (typeof fn !== "function") {
    fail(
      `serializeComponent is not exported from ${path.relative(REPO_ROOT, UTILS_TS)} — ` +
        `the capture cannot exercise the allow-list it is meant to guard.`,
    );
  }
  return fn;
}

// --- 4. Capture ------------------------------------------------------------

function capture() {
  const component = readFixtureRegistration();
  const serializeComponent = loadSerializeComponent();
  const serialized = serializeComponent(component);

  // JSON.stringify drops `undefined`-valued keys, which is exactly the
  // encoding an unclassified action must have: ABSENT, never a fabricated
  // default. Round-tripping here makes the committed fixture byte-identical to
  // what the IPC layer puts on the wire.
  const body = JSON.parse(
    JSON.stringify({
      success: true,
      data: { components: [serialized] },
    }),
  );

  return body;
}

/**
 * Turn a drift into an ACTIONABLE drift. A body that lost `effect` looks, in a
 * raw diff, like any other whitespace-ish change; naming the two places the
 * field is dropped from saves the reader the investigation that produced this
 * script in the first place.
 */
function effectDiagnostics(body) {
  const captured = body.data.components[0].actions;
  const notes = [];
  for (const [actionId, expected] of Object.entries(EXPECTED_EFFECTS)) {
    const action = captured.find((a) => a.id === actionId);
    if (!action) {
      notes.push(`action \`${actionId}\` is not in the capture at all`);
      continue;
    }
    if (action.effect === undefined) {
      notes.push(
        `\`${actionId}.effect\` is ABSENT from the capture (expected "${expected}"). Either the ` +
          `annotation was removed from ${path.relative(REPO_ROOT, SETTINGS_TSX)}, or ` +
          `\`effect: a.effect\` was removed from serializeComponent's CLOSED per-action ` +
          `allow-list in ${path.relative(REPO_ROOT, UTILS_TS)} — see ` +
          `src-tauri/src/mcp/ui_bridge/CONTRACT.md, "serializeComponent field allow-list"`,
      );
    } else if (action.effect !== expected) {
      notes.push(`\`${actionId}.effect\` is "${action.effect}", was "${expected}"`);
    }
  }
  return notes;
}

const body = capture();
const rendered = JSON.stringify(body, null, 2) + "\n";

if (UPDATE) {
  fs.mkdirSync(path.dirname(FIXTURE), { recursive: true });
  fs.writeFileSync(FIXTURE, rendered);
  console.log(
    `component-effect-fixture:update — wrote ${body.data.components[0].actions.length} actions ` +
      `for \`${FIXTURE_COMPONENT_ID}\` to ${path.relative(REPO_ROOT, FIXTURE)}`,
  );
  process.exit(0);
}

if (!fs.existsSync(FIXTURE)) {
  fail(
    `${path.relative(REPO_ROOT, FIXTURE)} is missing. Run ` +
      `\`node scripts/capture-component-effect-fixture.cjs --update\` and commit it.`,
  );
}

const committed = fs.readFileSync(FIXTURE, "utf8");
if (committed !== rendered) {
  console.error("");
  console.error(
    "ERROR: the captured /control/components body drifted from the committed fixture.",
  );
  console.error(
    "       Something changed in Settings.tsx's registration or in serializeComponent's",
  );
  console.error(
    "       per-action allow-list. Confirm the change is intended, then run",
  );
  console.error("       `node scripts/capture-component-effect-fixture.cjs --update` and commit.");
  for (const note of effectDiagnostics(body)) console.error(`   ! ${note}`);
  console.error("");
  console.error("--- committed ---");
  console.error(committed);
  console.error("--- captured ---");
  console.error(rendered);
  process.exit(1);
}

console.log(
  `component-effect-fixture OK — \`${FIXTURE_COMPONENT_ID}\` captured through the real ` +
    `serializeComponent matches ${path.relative(REPO_ROOT, FIXTURE)} ` +
    `(${Object.keys(EXPECTED_EFFECTS).length} effect annotations carried)`,
);
