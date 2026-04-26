#!/usr/bin/env node
// Cross-language action-name validator.
//
// The runner's `dispatch_app_request(state, "<action>", ...)` (and legacy
// `try_ws_dispatch(state, "<action>", payload)`) call sites in sdk_client.rs
// send `<action>` as the WS frame's action field. The wrapper SDK's
// HandlerRegistry dispatches each frame to the handler keyed by that string
// verbatim — a typo or rename on either side fails silently with NO_HANDLER
// at runtime.
//
// This script extracts every action name from both files and reports any
// runner-side action that has no matching handler in the wrapper SDK's
// relay-handlers.ts vocabulary. Exits non-zero on drift so it's safe to
// call from CI / pre-commit.

import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const repoRoot = resolve(__dirname, "..", "..");

const sdkClientPath = resolve(
  repoRoot,
  "qontinui-runner",
  "src-tauri",
  "src",
  "mcp",
  "sdk_client.rs",
);
const relayHandlersPath = resolve(
  repoRoot,
  "ui-bridge",
  "packages",
  "ui-bridge",
  "src",
  "server",
  "relay-handlers.ts",
);

function readFile(path) {
  try {
    return readFileSync(path, "utf8");
  } catch (err) {
    console.error(`fatal: cannot read ${path}: ${err.message}`);
    process.exit(2);
  }
}

// Match both:
//   dispatch_app_request(&state, "<name>", ...
//   try_ws_dispatch(&state, "<name>", ...
// `\s*` between tokens crosses newlines, so multi-line call shapes like
//   dispatch_app_request(
//       &state,
//       "<name>",
//       ...
//   )
// are matched too. The action string is always the second argument as a
// "double-quoted" literal.
function extractRunnerActions(src) {
  const patterns = [
    /dispatch_app_request\(\s*&state,\s*"([^"]+)"/g,
    /try_ws_dispatch\(\s*&state,\s*"([^"]+)"/g,
  ];
  const out = new Set();
  for (const re of patterns) {
    for (const m of src.matchAll(re)) out.add(m[1]);
  }
  return out;
}

// Match the wrapper-SDK relay vocabulary. Three call shapes coexist in
// relay-handlers.ts:
//   relayCommand('<name>'[, ...])
//   relay.queueCommand<...>('<name>'[, ...])
//   relayWithFallback('<name>'[, ...])
function extractWrapperActions(src) {
  const patterns = [
    /\brelayCommand(?:<[^>]*>)?\(\s*['"]([A-Za-z0-9_]+)['"]/g,
    /\.queueCommand(?:<[^>]*>)?\(\s*['"]([A-Za-z0-9_]+)['"]/g,
    /\brelayWithFallback(?:<[^>]*>)?\(\s*['"]([A-Za-z0-9_]+)['"]/g,
  ];
  const out = new Set();
  for (const re of patterns) {
    for (const m of src.matchAll(re)) out.add(m[1]);
  }
  return out;
}

const runnerActions = extractRunnerActions(readFile(sdkClientPath));
const wrapperActions = extractWrapperActions(readFile(relayHandlersPath));

if (runnerActions.size === 0) {
  console.error(
    "fatal: no dispatch_app_request or try_ws_dispatch call sites found in sdk_client.rs",
  );
  process.exit(2);
}
if (wrapperActions.size === 0) {
  console.error("fatal: no relay-handler action names found in relay-handlers.ts");
  process.exit(2);
}

const missing = [...runnerActions].filter((a) => !wrapperActions.has(a)).sort();
const orphans = [...wrapperActions].filter((a) => !runnerActions.has(a)).sort();

console.log(
  `runner sites=${runnerActions.size}, wrapper actions=${wrapperActions.size}, ` +
    `missing=${missing.length}, orphans=${orphans.length}`,
);

if (missing.length > 0) {
  console.error(
    "\nERROR: runner dispatches actions the wrapper SDK has no handler for:",
  );
  for (const a of missing) console.error(`  - ${a}`);
  console.error(
    "\nA WS-transport wrapper will fail with NO_HANDLER for these actions.",
  );
  console.error(
    "Either add the handler in ui-bridge/packages/ui-bridge/src/server/relay-handlers.ts,",
  );
  console.error(
    "or change the runner-side action name in sdk_client.rs to match an existing handler.",
  );
  process.exit(1);
}

// Orphans are informational only — a wrapper-SDK action with no runner caller
// just means the runner doesn't proxy that surface yet. Not an error.
if (process.argv.includes("--verbose") && orphans.length > 0) {
  console.log("\nwrapper actions with no runner caller (informational):");
  for (const a of orphans) console.log(`  - ${a}`);
}

console.log("ok: every runner action has a matching wrapper handler");
