#!/usr/bin/env node
// Regenerate bundled-page projections (`spec.uibridge.json`) from the IR
// documents (`state-machine.derived.json`) for every page under
// `qontinui-runner/specs/pages/`. Drives the Spec API's projection storage
// from outside the Rust runtime so we have a Node-side reference for
// byte-stability checks.
//
// Usage:
//   node specs/scripts/generate-bundled.mjs            # all pages
//   node specs/scripts/generate-bundled.mjs <pageId>   # one page

import { readFileSync, writeFileSync, readdirSync, statSync, existsSync } from "node:fs";
import { join, dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { projectIRToBundledPage } from "@qontinui/shared-types/ui-bridge-ir";

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const SPECS_ROOT = resolve(__dirname, "..");
const PAGES_DIR = join(SPECS_ROOT, "pages");

function listPages() {
  if (!existsSync(PAGES_DIR)) return [];
  return readdirSync(PAGES_DIR).filter((name) => {
    const p = join(PAGES_DIR, name);
    return statSync(p).isDirectory();
  });
}

function readNotes(pageDir) {
  const notesPath = join(pageDir, "notes.md");
  if (!existsSync(notesPath)) return undefined;
  const text = readFileSync(notesPath, "utf-8").trim();
  return text.length > 0 ? text : undefined;
}

function generate(pageId) {
  const pageDir = join(PAGES_DIR, pageId);
  const irPath = join(pageDir, "state-machine.derived.json");
  if (!existsSync(irPath)) {
    console.error(`No IR document for page '${pageId}' at ${irPath}`);
    process.exit(1);
  }
  const ir = JSON.parse(readFileSync(irPath, "utf-8"));
  const notes = readNotes(pageDir);
  const projected = projectIRToBundledPage(ir, notes);
  const outPath = join(pageDir, "spec.uibridge.json");
  // Pretty-print with 2-space indent and trailing newline. The projection
  // already sorts keys deterministically, so the output is byte-stable for
  // a given input.
  writeFileSync(outPath, JSON.stringify(projected, null, 2) + "\n", "utf-8");
  console.log(`Wrote ${outPath}`);
}

const target = process.argv[2];
if (target) {
  generate(target);
} else {
  for (const pageId of listPages()) generate(pageId);
}
