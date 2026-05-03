/**
 * Static guard against intra-spec elementId duplicates.
 *
 * The runtime state-machine compiler (`src/lib/compile-state-machine.ts`)
 * enforces the one-state-per-element invariant: within a single spec, no
 * element key may appear in `state.elements` of more than one state.
 * Violations cause `compileStateMachineFromSpecs` to return
 * `{ compiled: false, reason: "quarantined" }`, which makes
 * `window.__UI_BRIDGE__.stateMachine` undefined at runtime.
 *
 * The dedup commit at runner main `2d2430bfa` (Stage 2 of the SM
 * quarantine plan) cleared 54 such duplicates across 23 spec files. This
 * test exists to make sure AI-generated specs never silently re-introduce
 * them — without it, the next regression only surfaces when someone
 * notices the SM is missing in a UI Bridge probe.
 *
 * Mechanism: walk every `*.spec.uibridge.json` in this directory, run each
 * `state.elements[]` through the SAME `convertElementCriteria` +
 * `elementQueryKey` helpers the runtime compiler uses, and assert no key
 * is shared across two states inside one spec.
 *
 * Source of truth for the canonical-form helpers is
 * `src/lib/compile-state-machine.ts` — this test imports them directly so
 * a future change to the runtime canonicalisation can't make this test
 * pass while the compiler still quarantines.
 */

import { readdirSync, readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, it, expect } from "vitest";
import {
  convertElementCriteria,
  elementQueryKey,
} from "../lib/compile-state-machine";
import type { SpecConfig } from "../lib/spec-prompt-builder";

const SPECS_DIR = dirname(fileURLToPath(import.meta.url));

interface DuplicateReport {
  specFile: string;
  elementKey: string;
  stateIds: string[];
}

/**
 * For one spec, return a list of every element key claimed by 2+ states
 * inside the same spec. Empty list = clean.
 */
function findIntraSpecDuplicates(spec: SpecConfig): Array<{
  elementKey: string;
  stateIds: string[];
}> {
  const states = spec.stateMachine?.states;
  if (!states || states.length === 0) return [];

  // Map of element-query-key → set of state IDs that claim it.
  const keyToStates = new Map<string, Set<string>>();

  for (const state of states) {
    if (!Array.isArray(state.elements)) continue;
    // Per-state dedupe: a state listing the same element twice is fine,
    // only cross-state duplicates count. Mirrors the
    // `byState`/`scopedKey` logic in compile-state-machine.ts.
    const seenInThisState = new Set<string>();
    for (const element of state.elements) {
      const query = convertElementCriteria(element);
      const key = elementQueryKey(query);
      if (!key) continue;
      if (seenInThisState.has(key)) continue;
      seenInThisState.add(key);

      const bucket = keyToStates.get(key) ?? new Set<string>();
      bucket.add(state.id);
      keyToStates.set(key, bucket);
    }
  }

  const dupes: Array<{ elementKey: string; stateIds: string[] }> = [];
  for (const [key, stateIds] of keyToStates.entries()) {
    if (stateIds.size < 2) continue;
    dupes.push({ elementKey: key, stateIds: [...stateIds].sort() });
  }
  return dupes;
}

describe("spec files: intra-spec elementId uniqueness", () => {
  const specFiles = readdirSync(SPECS_DIR)
    .filter((f) => f.endsWith(".spec.uibridge.json"))
    .sort();

  // Sanity check — if this drops to zero the glob is broken and the test
  // would silently pass on every spec.
  it("discovers at least one spec file", () => {
    expect(specFiles.length).toBeGreaterThan(0);
  });

  it("has no element keys claimed by multiple states inside any single spec", () => {
    const allDuplicates: DuplicateReport[] = [];
    for (const file of specFiles) {
      // Strip a UTF-8 BOM (U+FEFF) if present — at least one historical
      // spec (settings-security.spec.uibridge.json) shipped with one and
      // we don't want a static-validator failure to depend on a separate
      // encoding fix.
      let raw = readFileSync(join(SPECS_DIR, file), "utf8");
      if (raw.charCodeAt(0) === 0xfeff) raw = raw.slice(1);
      const spec = JSON.parse(raw) as SpecConfig;
      const dupes = findIntraSpecDuplicates(spec);
      for (const d of dupes) {
        allDuplicates.push({
          specFile: file,
          elementKey: d.elementKey,
          stateIds: d.stateIds,
        });
      }
    }

    if (allDuplicates.length > 0) {
      const lines = allDuplicates.map(
        (d) =>
          `  ${d.specFile}\n    elementKey: ${d.elementKey}\n    states:     ${d.stateIds.join(", ")}`,
      );
      const message =
        `Found ${allDuplicates.length} intra-spec element duplicate(s). ` +
        `Each entry below is one element key claimed by 2+ states inside the ` +
        `SAME spec, which the runtime SM compiler will quarantine — making ` +
        `\`window.__UI_BRIDGE__.stateMachine\` undefined.\n\n` +
        `Fix: drop the offending element from every state but one (the most ` +
        `discriminating). See runner commit 2d2430bfa for the precedent.\n\n` +
        lines.join("\n\n");
      // Throwing produces a much more readable message than expect()
      // because vitest dumps the full string verbatim instead of a diff.
      throw new Error(message);
    }

    expect(allDuplicates).toEqual([]);
  });
});
