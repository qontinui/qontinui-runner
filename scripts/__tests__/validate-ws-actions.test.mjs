#!/usr/bin/env node
// Regression tests for validate-ws-actions.mjs's extractors and CLI.
//
// Uses Node's built-in `node:test` runner (vitest only globs `src/**`), like
// the other scripts/__tests__ files. Run with:
//   node --test scripts/__tests__/validate-ws-actions.test.mjs
//
// The fixture sdk_client.rs.txt holds one handler per call shape the extractor
// has to recognise. Each was missed by an earlier version of the patterns, and
// a miss is silent: the action-vocabulary doc just loses rows and the
// `missing=` check runs over an incomplete set. The cases:
//   (a) `dispatch_app_request_typed(` and `dispatch_app_request_by_id(` sites
//   (b) handlers declared `pub(crate) async fn` / `pub async fn`
//   (c) a `/// METHOD /path` line followed by further `///` lines
//   (d) the CLI's missing-handler check seeing the (a) sites
// The fixtures carry a `.txt` suffix so no repo hook treats them as Rust or TS.

import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
  buildDocSection,
  extractRouteTable,
  extractRunnerActions,
  extractWrapperActions,
} from "../validate-ws-actions.mjs";

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const SCRIPT = resolve(__dirname, "..", "validate-ws-actions.mjs");
const FIXTURES = resolve(__dirname, "..", "tests", "fixtures", "validate-ws-actions");
const SDK_SRC = readFileSync(join(FIXTURES, "sdk_client.rs.txt"), "utf8");
const RELAY_SRC = readFileSync(join(FIXTURES, "relay-handlers.ts.txt"), "utf8");

const ALL_RUNNER_ACTIONS = [
  "byIdAction",
  "crateVisibleAction",
  "legacyAction",
  "longDocAction",
  "plainAction",
  "publicAction",
  "typedAction",
  "undocumentedAction",
];

test("extractRunnerActions sees every dispatch entry point", () => {
  assert.deepEqual([...extractRunnerActions(SDK_SRC)].sort(), ALL_RUNNER_ACTIONS);
});

test("(a) _typed and _by_id sites are runner actions", () => {
  const actions = extractRunnerActions(SDK_SRC);
  assert.ok(actions.has("typedAction"), "dispatch_app_request_typed( site missed");
  assert.ok(actions.has("byIdAction"), "dispatch_app_request_by_id( site missed");
  // The _by_id form's second argument is the app id, never the action.
  assert.ok(!actions.has("app_id"));
});

test("extractWrapperActions reads the relay vocabulary", () => {
  assert.deepEqual(
    [...extractWrapperActions(RELAY_SRC)].sort(),
    [...ALL_RUNNER_ACTIONS, "wrapperOnlyAction"].sort(),
  );
});

function routeFor(handler) {
  return extractRouteTable(SDK_SRC).find((r) => r.handler === handler);
}

test("route table rows: one per documented handler with a dispatch site", () => {
  const rows = extractRouteTable(SDK_SRC);
  assert.deepEqual(rows.map((r) => r.handler).sort(), [
    "handle_by_id",
    "handle_crate_visible",
    "handle_legacy",
    "handle_long_doc",
    "handle_plain",
    "handle_public",
    "handle_typed",
  ]);
});

test("(a) route table pairs _typed and _by_id handlers with their action", () => {
  assert.deepEqual(routeFor("handle_typed"), {
    method: "POST",
    path: "/ui-bridge/sdk/typed",
    desc: "(a) the _typed entry point",
    handler: "handle_typed",
    action: "typedAction",
  });
  assert.equal(routeFor("handle_by_id")?.action, "byIdAction");
  assert.equal(routeFor("handle_by_id")?.path, "/ui-bridge/sdk/by-id/:app_id");
});

test("(b) pub(crate) and pub handlers are in the route table", () => {
  assert.equal(routeFor("handle_crate_visible")?.action, "crateVisibleAction");
  assert.equal(routeFor("handle_crate_visible")?.method, "DELETE");
  assert.equal(routeFor("handle_public")?.action, "publicAction");
});

test("(c) a route line followed by more doc lines keeps only the first as desc", () => {
  const row = routeFor("handle_long_doc");
  assert.equal(row?.action, "longDocAction");
  assert.equal(row?.method, "PATCH");
  assert.equal(row?.desc, "(c) route line followed by more doc lines");
});

test("a handler with no dispatch site does not borrow the next handler's", () => {
  // handle_ipc_only is followed by a `pub(crate) async fn` whose body has a
  // dispatch site; the body scan must stop at that signature.
  assert.equal(routeFor("handle_ipc_only"), undefined);
  assert.ok(!extractRouteTable(SDK_SRC).some((r) => r.action === "undocumentedAction"));
});

test("doc header names the workspace-relative regenerate path", () => {
  const section = buildDocSection(extractRouteTable(SDK_SRC));
  assert.match(section, /_7 routes; /);
  assert.match(section, /--emit-doc ui-bridge\/docs-site\/docs\/wrappers\/action-vocabulary\.md/);
  assert.doesNotMatch(section, /--emit-doc docs-site\//);
});

// ---------------------------------------------------------------------------
// CLI: the script resolves its inputs against <script>/../.., so copy it into a
// throwaway workspace with the fixtures at the sibling-checkout paths.
// ---------------------------------------------------------------------------

function makeWorkspace(relaySrc) {
  const root = mkdtempSync(join(tmpdir(), "validate-ws-actions-"));
  const scriptDir = join(root, "qontinui-runner", "scripts");
  const sdkDir = join(root, "qontinui-runner", "src-tauri", "src", "mcp");
  const relayDir = join(root, "ui-bridge", "packages", "ui-bridge", "src", "server");
  const docDir = join(root, "ui-bridge", "docs-site", "docs", "wrappers");
  for (const d of [scriptDir, sdkDir, relayDir, docDir]) mkdirSync(d, { recursive: true });
  copyFileSync(SCRIPT, join(scriptDir, "validate-ws-actions.mjs"));
  writeFileSync(join(sdkDir, "sdk_client.rs"), SDK_SRC);
  writeFileSync(join(relayDir, "relay-handlers.ts"), relaySrc);
  writeFileSync(join(docDir, "action-vocabulary.md"), "# Action Vocabulary\n");
  return root;
}

function runCli(root, ...args) {
  return spawnSync(
    process.execPath,
    [join(root, "qontinui-runner", "scripts", "validate-ws-actions.mjs"), ...args],
    { encoding: "utf8" },
  );
}

test("CLI: full wrapper coverage passes and counts every runner site", () => {
  const root = makeWorkspace(RELAY_SRC);
  try {
    const r = runCli(root);
    assert.equal(r.status, 0, r.stderr);
    assert.match(r.stdout, /runner sites=8, wrapper actions=9, missing=0, orphans=1/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("(d) CLI: a wrapper missing a _typed/_by_id action fails the check", () => {
  const relay = RELAY_SRC.split("\n")
    .filter((l) => !l.includes("'typedAction'") && !l.includes("'byIdAction'"))
    .join("\n");
  const root = makeWorkspace(relay);
  try {
    const r = runCli(root);
    assert.equal(r.status, 1, r.stdout);
    assert.match(r.stdout, /missing=2/);
    assert.match(r.stderr, /- byIdAction\n\s+- typedAction/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("CLI: --emit-doc then --check-doc round-trips on the workspace-relative path", () => {
  const root = makeWorkspace(RELAY_SRC);
  const doc = "ui-bridge/docs-site/docs/wrappers/action-vocabulary.md";
  try {
    const emit = runCli(root, "--emit-doc", doc);
    assert.equal(emit.status, 0, emit.stderr);
    assert.match(emit.stdout, /wrote: .* \(7 routes\)/);
    const check = runCli(root, "--check-doc", doc);
    assert.equal(check.status, 0, check.stderr);
    const written = readFileSync(join(root, doc), "utf8");
    assert.match(written, /\| `crateVisibleAction` \| `DELETE \/ui-bridge\/sdk\/crate-visible`/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
