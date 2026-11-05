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

import type { PaneClaudeProbe } from "./resumeVerification";
import {
  findTerminalsOutsideProject,
  moveTerminalsToProject,
  readBareIdleShell,
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

/**
 * "Move them" replaces ONLY a pane one process-table read shows is a bare idle
 * shell (its root a shell, nothing under it), and keeps every other pane —
 * named, with its reason. The `node` case is the one an image-name test misses:
 * an npm-installed Claude Code runs as `node … cli.js`, so the probe reads
 * `absent`, and only the shell's descendant count sees it.
 */
describe("moveTerminalsToProject", () => {
  const ELSEWHERE = "D:\\projects\\book-notes";
  const shell = (descendants: number): PaneClaudeProbe => ({
    state: "absent",
    sessionIds: [],
    root: { image: "bash", isShell: true, descendants },
  });
  const probes: Record<string, PaneClaudeProbe> = {
    bare: shell(0),
    claude: {
      state: "live",
      sessionIds: [],
      root: { image: "bash", isShell: true, descendants: 1 },
    },
    // `node … cli.js` under the shell: no process NAMED claude, so `absent`.
    npm: shell(1),
    // A pane whose root is some other program (here `vim`), no children.
    other: {
      state: "absent",
      sessionIds: [],
      root: { image: "vim", isShell: false, descendants: 0 },
    },
    unreadable: { state: "unknown", sessionIds: [] },
  };
  const outside = Object.keys(probes).map((id) =>
    tab({ id, title: `T-${id}`, workingDir: ELSEWHERE }),
  );

  function run(probeOf: (id: string) => PaneClaudeProbe = (id) => probes[id]) {
    const created: string[] = [];
    const closed: string[] = [];
    const outcome = moveTerminalsToProject(outside, ROOT, {
      probe: async (id) => probeOf(id),
      createTerminal: async (title, workingDir) => {
        created.push(`${title}@${workingDir}`);
        return `replacement-of-${title}`;
      },
      closeTerminal: (id) => {
        closed.push(id);
      },
    });
    return { outcome, created, closed };
  }

  it("replaces the bare idle shell and keeps the claude, node, other-process and unreadable panes", async () => {
    const { outcome, created, closed } = run();
    const result = await outcome;
    expect(result.replaced).toEqual(["bare"]);
    expect(created).toEqual([`T-bare@${ROOT}`]);
    expect(closed).toEqual(["bare"]);
    expect(result.kept.map((k) => [k.id, k.reason])).toEqual([
      ["claude", "claude is running in it"],
      ["npm", "1 process is running in it"],
      ["other", "vim is running in it"],
      ["unreadable", "its processes could not be read"],
    ]);
    expect(result.failed).toEqual([]);
  });

  it("keeps a bare shell that started something while its replacement opened, and closes the replacement", async () => {
    let reads = 0;
    const { outcome, closed } = run((id) => {
      if (id !== "bare") return probes[id];
      reads += 1;
      return reads === 1 ? shell(0) : shell(1);
    });
    const result = await outcome;
    expect(result.replaced).toEqual([]);
    expect(closed).toEqual(["replacement-of-T-bare"]);
    expect(result.kept[0]).toEqual({
      id: "bare",
      title: "T-bare",
      reason: "1 process is running in it (it started during the move)",
    });
  });

  it("leaves a bare shell running when its replacement cannot be opened", async () => {
    const closed: string[] = [];
    const result = await moveTerminalsToProject(
      [tab({ id: "bare", workingDir: ELSEWHERE })],
      ROOT,
      {
        probe: async () => shell(0),
        createTerminal: async () => null,
        closeTerminal: (id) => {
          closed.push(id);
        },
      },
    );
    expect(closed).toEqual([]);
    expect(result.failed.map((f) => f.id)).toEqual(["bare"]);
  });
});

describe("readBareIdleShell", () => {
  it("needs the root half of the answer — an older runner's bare `absent` is not enough", () => {
    expect(readBareIdleShell({}, { state: "absent", sessionIds: [] })).toEqual({
      bare: false,
      reason: "its processes could not be read",
    });
  });

  it("never reads a remote pane as bare", () => {
    expect(
      readBareIdleShell(
        { remote: { deviceId: "d" } },
        { state: "absent", sessionIds: [], root: { image: "bash", isShell: true, descendants: 0 } },
      ).bare,
    ).toBe(false);
    expect(readBareIdleShell({}, { state: "remote", sessionIds: [] }).bare).toBe(false);
  });

  it("keeps a pane whose root process is gone from the table", () => {
    expect(
      readBareIdleShell(
        {},
        { state: "absent", sessionIds: [], root: { image: null, isShell: false, descendants: 0 } },
      ),
    ).toEqual({ bare: false, reason: "its process could not be found" });
  });
});
