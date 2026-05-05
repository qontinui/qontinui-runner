/**
 * IR-overlap commit filter (Section 11, Phase A3, Decision #4).
 *
 * Pure function that narrows a list of recent git commits to the subset whose
 * `files[]` intersects the source files referenced by an IR document's state
 * + transition provenance. This is the natural file-overlap bound on which
 * commits can plausibly explain a regression failure — there is no count cap
 * (the plan calls this out explicitly).
 *
 * Determinism: stable output ordering inherited from the input arrays;
 * commits are kept in the caller's supplied order.
 */

import type { IRDocument } from "@qontinui/shared-types/ui-bridge-ir";
import type { GitCommitRef } from "@qontinui/ui-bridge-auto/drift";

/**
 * Collect the set of source file paths declared by every IR state and
 * transition's `provenance.file` field. States or transitions without a
 * `provenance.file` contribute nothing — that is the documented signal that
 * the IR node has no on-disk source we can correlate against.
 */
function collectIrSourceFiles(ir: IRDocument): Set<string> {
  const files = new Set<string>();
  for (const state of ir.states) {
    const file = state.provenance?.file;
    if (typeof file === "string" && file.length > 0) {
      files.add(file);
    }
  }
  for (const transition of ir.transitions) {
    const file = transition.provenance?.file;
    if (typeof file === "string" && file.length > 0) {
      files.add(file);
    }
  }
  return files;
}

/**
 * Filter recent commits to those whose `files[]` shares at least one path
 * with the IR's collected source files.
 *
 * If the IR has no source files (no provenance anywhere), returns an empty
 * array — the callers' diagnose path then runs without commit hypotheses,
 * which is the correct graceful-degradation behavior.
 *
 * @param ir — the IR document the regression suite was generated from.
 * @param commits — recent git commits (already time-windowed by the caller).
 *   The function does not re-window; it only file-overlap-filters.
 * @returns commits whose changed files intersect the IR's source files.
 *   Order is preserved from the input.
 */
export function filterCommitsByIrOverlap(ir: IRDocument, commits: GitCommitRef[]): GitCommitRef[] {
  const irFiles = collectIrSourceFiles(ir);
  if (irFiles.size === 0) return [];

  const out: GitCommitRef[] = [];
  for (const commit of commits) {
    let overlaps = false;
    for (const file of commit.files) {
      if (irFiles.has(file)) {
        overlaps = true;
        break;
      }
    }
    if (overlaps) out.push(commit);
  }
  return out;
}
