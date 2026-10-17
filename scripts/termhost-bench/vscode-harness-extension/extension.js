"use strict";
/**
 * termhost-bench VS Code harness extension — the entry point VS Code loads with
 * `--extensionDevelopmentPath` (plan `2026-10-11-terminal-page-performance-kpis-across-setups`,
 * Phase 4). Plain JavaScript, no dependencies, no build step; `vscode` is the
 * API module the extension host provides at runtime.
 *
 * It does nothing unless the isolated instance was launched by the harness:
 * `adapters/vscode.mjs` names a per-launch config file in
 * `TERMHOST_BENCH_HARNESS_CONFIG` (nonce, control-file path, the session
 * shell). Without that variable no endpoint is opened. Everything else lives in
 * `harness.js` (controller + endpoint) and `protocol.js` (pure protocol and
 * arrangement planning), which the Node tests exercise without VS Code.
 */

const vscode = require("vscode");

const { startHarness } = require("./harness.js");

let running = null;

async function activate(context) {
  try {
    running = await startHarness({ vscode, env: process.env });
  } catch (e) {
    // Surfaced to the adapter as a control file that never appears; the
    // extension host's own log keeps the reason.
    console.error(`termhost-bench harness failed to start: ${e?.stack ?? e}`);
    return;
  }
  if (running) context.subscriptions.push({ dispose: () => running?.dispose() });
}

function deactivate() {
  const r = running;
  running = null;
  return r?.dispose();
}

module.exports = { activate, deactivate };
