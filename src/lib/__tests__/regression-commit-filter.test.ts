/**
 * Tests for `filterCommitsByIrOverlap`.
 *
 * Pure unit tests against the IR-overlap commit filter (Section 11, Decision
 * #4). The filter narrows recent git commits to those whose `files[]` shares
 * at least one path with the IR's source files (collected from each state's
 * + transition's `provenance.file` field).
 */

import { describe, it, expect } from "vitest";

import type { IRDocument } from "@qontinui/shared-types/ui-bridge-ir";
import type { GitCommitRef } from "@qontinui/ui-bridge-auto/drift";

import { filterCommitsByIrOverlap } from "../regression-commit-filter";

// ---------------------------------------------------------------------------
// Fixture builders
// ---------------------------------------------------------------------------

function makeIr(
  stateFiles: Array<string | undefined>,
  transitionFiles: Array<string | undefined>,
): IRDocument {
  return {
    version: "1.0",
    id: "test-ir",
    name: "Test IR",
    states: stateFiles.map((file, i) => ({
      id: `state-${i}`,
      name: `State ${i}`,
      assertions: [],
      ...(file !== undefined ? { provenance: { source: "build-plugin" as const, file } } : {}),
    })),
    transitions: transitionFiles.map((file, i) => ({
      id: `transition-${i}`,
      name: `Transition ${i}`,
      fromStates: [],
      activateStates: [],
      actions: [],
      ...(file !== undefined ? { provenance: { source: "build-plugin" as const, file } } : {}),
    })),
  };
}

function makeCommit(sha: string, files: string[]): GitCommitRef {
  return {
    sha,
    message: `commit ${sha}`,
    author: "test",
    timestamp: 0,
    files,
  };
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("filterCommitsByIrOverlap", () => {
  it("returns empty when IR has no provenance files", () => {
    const ir = makeIr([undefined, undefined], [undefined]);
    const commits = [makeCommit("abc", ["src/foo.ts"])];
    expect(filterCommitsByIrOverlap(ir, commits)).toEqual([]);
  });

  it("returns empty when no commits intersect IR files", () => {
    const ir = makeIr(["src/foo.ts"], ["src/bar.ts"]);
    const commits = [
      makeCommit("abc", ["src/baz.ts"]),
      makeCommit("def", ["src/qux.ts", "test/spec.ts"]),
    ];
    expect(filterCommitsByIrOverlap(ir, commits)).toEqual([]);
  });

  it("keeps commits whose files intersect any state's provenance file", () => {
    const ir = makeIr(["src/foo.ts", "src/bar.ts"], []);
    const commits = [
      makeCommit("abc", ["src/foo.ts", "src/other.ts"]),
      makeCommit("def", ["src/bar.ts"]),
      makeCommit("ghi", ["src/unrelated.ts"]),
    ];
    const out = filterCommitsByIrOverlap(ir, commits);
    expect(out.map((c) => c.sha)).toEqual(["abc", "def"]);
  });

  it("keeps commits whose files intersect any transition's provenance file", () => {
    const ir = makeIr([], ["src/transitions.ts"]);
    const commits = [
      makeCommit("abc", ["src/transitions.ts"]),
      makeCommit("def", ["src/elsewhere.ts"]),
    ];
    const out = filterCommitsByIrOverlap(ir, commits);
    expect(out.map((c) => c.sha)).toEqual(["abc"]);
  });

  it("preserves caller-supplied commit order", () => {
    const ir = makeIr(["src/foo.ts"], ["src/bar.ts"]);
    const commits = [
      makeCommit("c3", ["src/foo.ts"]),
      makeCommit("c1", ["src/bar.ts"]),
      makeCommit("c2", ["src/foo.ts", "src/bar.ts"]),
    ];
    const out = filterCommitsByIrOverlap(ir, commits);
    expect(out.map((c) => c.sha)).toEqual(["c3", "c1", "c2"]);
  });

  it("treats empty / undefined provenance.file as no signal", () => {
    const ir = makeIr(["", undefined], ["src/real.ts"]);
    const commits = [makeCommit("abc", [""]), makeCommit("def", ["src/real.ts"])];
    const out = filterCommitsByIrOverlap(ir, commits);
    expect(out.map((c) => c.sha)).toEqual(["def"]);
  });
});
