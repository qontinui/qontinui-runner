/**
 * Tests for `findTerminalsOutsideProject` — the detection half of the
 * "N terminals are in a different folder" chip (projects-dashboard plan §7.2
 * step 4).
 *
 * The chip's [Move them] action CLOSES the terminals this function reports,
 * so a false positive costs the operator a running shell. Every case below is
 * about NOT reporting something we cannot prove is in the wrong folder.
 *
 * vitest runs `environment: "node"` with no React Testing Library, so the pure
 * helper is exercised directly (same precedent as `reconcilePages`).
 */

import { describe, it, expect } from "vitest";

import {
  findTerminalsOutsideProject,
  type ReconcilableTerminal,
} from "./useProjectTerminalReconcile";

const ROOT = "D:\\projects\\pizzeria";

function tab(over: Partial<ReconcilableTerminal> = {}): ReconcilableTerminal {
  return { id: "t1", title: "Terminal 1", isAlive: true, workingDir: ROOT, ...over };
}

describe("findTerminalsOutsideProject", () => {
  it("reports a live terminal whose cwd is a different folder", () => {
    const out = findTerminalsOutsideProject(
      [tab({ id: "in" }), tab({ id: "out", workingDir: "D:\\projects\\book-notes" })],
      ROOT,
    );
    expect(out.map((t) => t.id)).toEqual(["out"]);
  });

  it("does not report a terminal that is at the root written differently", () => {
    // Spawn cwd vs folder-picker root: separators, case and trailing slash all
    // differ but it is the same directory.
    const out = findTerminalsOutsideProject(
      [tab({ workingDir: "d:/PROJECTS/pizzeria/" })],
      "D:\\projects\\pizzeria",
    );
    expect(out).toEqual([]);
  });

  it("does not report a sibling-prefix directory as being at the root", () => {
    // Guard against a `startsWith`-style comparison: "D:\projects\pizzeria-old"
    // is NOT the root and must be reported.
    const out = findTerminalsOutsideProject([tab({ workingDir: `${ROOT}-old` })], ROOT);
    expect(out).toHaveLength(1);
  });

  it("never reports a terminal with an UNKNOWN cwd", () => {
    // An absent workingDir is not a known mismatch — offering to close it
    // would be acting on a guess.
    const out = findTerminalsOutsideProject(
      [tab({ workingDir: undefined }), tab({ id: "blank", workingDir: "   " })],
      ROOT,
    );
    expect(out).toEqual([]);
  });

  it("never reports plan tabs or exited tombstones", () => {
    const out = findTerminalsOutsideProject(
      [
        tab({ id: "plan", type: "plan", workingDir: "D:\\elsewhere" }),
        tab({ id: "dead", isAlive: false, workingDir: "D:\\elsewhere" }),
      ],
      ROOT,
    );
    expect(out).toEqual([]);
  });

  it("reports nothing when no project is active", () => {
    const tabs = [tab({ workingDir: "D:\\elsewhere" })];
    expect(findTerminalsOutsideProject(tabs, null)).toEqual([]);
    expect(findTerminalsOutsideProject(tabs, undefined)).toEqual([]);
    expect(findTerminalsOutsideProject(tabs, "   ")).toEqual([]);
  });
});
