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
import type { ShellPromptMark } from "./shellIntegrationEvents";
import {
  findTerminalsOutsideProject,
  moveTerminalsToProject,
  readBareIdleShell,
  summarizeLastMove,
  type ReconcilableTerminal,
} from "./useProjectTerminalReconcile";

const ROOT = "D:\\projects\\pizzeria";
const NO_PROMPT = "no prompt seen since this pane was shown — press Enter in it, then Check again";

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
 * "Move them" replaces ONLY a pane that two readings agree is a bare idle shell:
 * the process table (its root a shell, nothing under it) AND the shell's own
 * prompt state — a prompt mark (633;A) this window parsed while the pane was
 * live, from a shell that announced it marks every command start. Everything
 * else is kept — named, with its reason.
 *
 * Cases each reading alone misses:
 *  - `node … cli.js` (an npm-installed Claude Code) has no process NAMED
 *    claude, so the probe reads `absent`; only the shell's descendant count
 *    sees it.
 *  - PowerShell runs a cmdlet or script in-process (`Get-Content -Wait`, a
 *    `.ps1` loop): root = pwsh, ZERO descendants; only the prompt-state mark
 *    (633;C last) sees it.
 *  - PowerShell without PSReadLine draws prompts (633;A) and never marks a
 *    command start, so its prompt mark is no evidence; it announces no
 *    capability and is kept.
 */
describe("moveTerminalsToProject", () => {
  const ELSEWHERE = "D:\\projects\\book-notes";
  const shell = (descendants: number, image = "bash"): PaneClaudeProbe => ({
    state: "absent",
    sessionIds: [],
    root: { image, isShell: true, descendants },
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
    // `Get-Content -Wait` in pwsh: in-process, so no descendant at all.
    pwshBusy: shell(0, "pwsh.exe"),
    // A bare shell whose marks this window is not parsing (hidden, remounted).
    unmarked: shell(0),
    // PowerShell without PSReadLine: prompts, but no command-start marks.
    noReadline: shell(0, "pwsh.exe"),
  };
  const marks: Record<string, ShellPromptMark | undefined> = {
    bare: "prompt_start",
    claude: "command_execute",
    npm: "command_execute",
    other: undefined,
    unreadable: "prompt_start",
    pwshBusy: "command_execute",
    unmarked: undefined,
    noReadline: "prompt_start",
  };
  const announces = (id: string) => id !== "noReadline";
  const outside = Object.keys(probes).map((id) =>
    tab({ id, title: `T-${id}`, workingDir: ELSEWHERE }),
  );

  function run(
    probeOf: (id: string) => PaneClaudeProbe = (id) => probes[id],
    markOf: (id: string) => ShellPromptMark | undefined = (id) => marks[id],
  ) {
    const created: string[] = [];
    const closed: string[] = [];
    const outcome = moveTerminalsToProject(outside, ROOT, {
      probe: async (id) => probeOf(id),
      shellMark: markOf,
      announcesCommandStarts: announces,
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

  it("replaces only the bare shell at its prompt and keeps every busy, unread or unmarked pane", async () => {
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
      ["pwshBusy", "its shell last reported a command start"],
      ["unmarked", NO_PROMPT],
      ["noReadline", "its shell does not report command starts"],
    ]);
    expect(result.failed).toEqual([]);
  });

  it("keeps a pwsh pane with no descendant whose last mark is a command start", async () => {
    const pane = [tab({ id: "pwsh", title: "T-pwsh", workingDir: ELSEWHERE })];
    const closed: string[] = [];
    const created: string[] = [];
    const result = await moveTerminalsToProject(pane, ROOT, {
      probe: async () => shell(0, "pwsh.exe"),
      shellMark: () => "command_execute",
      announcesCommandStarts: () => true,
      createTerminal: async (title) => {
        created.push(title);
        return "r";
      },
      closeTerminal: (id) => {
        closed.push(id);
      },
    });
    expect(result.replaced).toEqual([]);
    expect(created).toEqual([]);
    expect(closed).toEqual([]);
    expect(result.kept).toEqual([
      { id: "pwsh", title: "T-pwsh", reason: "its shell last reported a command start" },
    ]);
  });

  it("keeps a bare shell that started a command while its replacement opened, and closes the replacement", async () => {
    let reads = 0;
    const { outcome, closed } = run(undefined, (id) => {
      if (id !== "bare") return marks[id];
      reads += 1;
      return reads === 1 ? "prompt_start" : "command_execute";
    });
    const result = await outcome;
    expect(result.replaced).toEqual([]);
    expect(closed).toEqual(["replacement-of-T-bare"]);
    expect(result.kept[0]).toEqual({
      id: "bare",
      title: "T-bare",
      reason: "its shell last reported a command start (it started during the move)",
    });
  });

  it("keeps a bare shell that spawned a process while its replacement opened", async () => {
    let reads = 0;
    const { outcome, closed } = run((id) => {
      if (id !== "bare") return probes[id];
      reads += 1;
      return reads === 1 ? shell(0) : shell(1);
    });
    const result = await outcome;
    expect(closed).toEqual(["replacement-of-T-bare"]);
    expect(result.kept[0].reason).toBe("1 process is running in it (it started during the move)");
  });

  it("does not claim something started when the second read only failed", async () => {
    let reads = 0;
    const { outcome, closed } = run((id) => {
      if (id !== "bare") return probes[id];
      reads += 1;
      return reads === 1 ? shell(0) : { state: "unknown", sessionIds: [] };
    });
    const result = await outcome;
    expect(closed).toEqual(["replacement-of-T-bare"]);
    expect(result.kept[0].reason).toBe("its processes could not be read");
  });

  it("keeps a bare shell whose marks went unparsed while its replacement opened", async () => {
    // The replacement became the active tab, hiding the original; a chunk it
    // printed was dropped unparsed, which clears its marks.
    let reads = 0;
    const { outcome, closed } = run(undefined, (id) => {
      if (id !== "bare") return marks[id];
      reads += 1;
      return reads === 1 ? "prompt_start" : undefined;
    });
    const result = await outcome;
    expect(closed).toEqual(["replacement-of-T-bare"]);
    expect(result.kept[0].reason).toBe(NO_PROMPT);
  });

  it("leaves a bare shell running when its replacement cannot be opened", async () => {
    const closed: string[] = [];
    const result = await moveTerminalsToProject(
      [tab({ id: "bare", workingDir: ELSEWHERE })],
      ROOT,
      {
        probe: async () => shell(0),
        shellMark: () => "prompt_start",
        announcesCommandStarts: () => true,
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
  const bareRoot = { image: "bash", isShell: true, descendants: 0 };
  const atPrompt = { mark: "prompt_start" as const, announcesCommandStarts: true };

  it("needs the root half of the answer — an older runner's bare `absent` is not enough", () => {
    expect(readBareIdleShell({}, { state: "absent", sessionIds: [] }, atPrompt)).toEqual({
      bare: false,
      reason: "its processes could not be read",
      busy: false,
    });
  });

  it("needs a prompt mark: a pane with no current mark is kept", () => {
    expect(
      readBareIdleShell(
        {},
        { state: "absent", sessionIds: [], root: bareRoot },
        { mark: undefined, announcesCommandStarts: true },
      ),
    ).toEqual({ bare: false, reason: NO_PROMPT, busy: false });
  });

  it("keeps a pane at its prompt whose shell never announced command-start marks", () => {
    expect(
      readBareIdleShell(
        {},
        { state: "absent", sessionIds: [], root: bareRoot },
        { mark: "prompt_start", announcesCommandStarts: false },
      ),
    ).toEqual({ bare: false, reason: "its shell does not report command starts", busy: false });
  });

  it("reads bare only with both halves: a shell with no descendant, at a prompt it marks", () => {
    expect(
      readBareIdleShell({}, { state: "absent", sessionIds: [], root: bareRoot }, atPrompt),
    ).toEqual({ bare: true });
  });

  it("never reads a remote pane as bare", () => {
    expect(
      readBareIdleShell(
        { remote: { deviceId: "d" } },
        { state: "absent", sessionIds: [], root: bareRoot },
        atPrompt,
      ).bare,
    ).toBe(false);
    expect(readBareIdleShell({}, { state: "remote", sessionIds: [] }, atPrompt).bare).toBe(false);
  });

  it("keeps a pane whose root process is gone from the table", () => {
    expect(
      readBareIdleShell(
        {},
        { state: "absent", sessionIds: [], root: { image: null, isShell: false, descendants: 0 } },
        atPrompt,
      ),
    ).toEqual({ bare: false, reason: "its process could not be found", busy: false });
  });
});

describe("summarizeLastMove", () => {
  const k = (id: string, reason = "claude is running in it") => ({ id, title: `T-${id}`, reason });
  const here = "d:/projects/pizzeria";

  it("says Check again only when every outside pane was kept — a failed replacement is not kept", () => {
    const outside = [{ id: "a" }, { id: "b" }];
    const allKept = summarizeLastMove(
      { root: here, kept: [k("a"), k("b")], failed: [] },
      here,
      outside,
    );
    expect(allKept.allKept).toBe(true);
    expect(allKept.error).toBeNull();

    const oneFailed = summarizeLastMove(
      { root: here, kept: [k("a")], failed: [k("b", "its replacement could not be opened")] },
      here,
      outside,
    );
    expect(oneFailed.allKept).toBe(false);
    expect(oneFailed.kept.map((x) => x.id)).toEqual(["a", "b"]);
    expect(oneFailed.error).toBe(
      "1 terminal could not be reopened at the project folder and was left where it is.",
    );
  });

  it("shows nothing from a move made for another project root", () => {
    const outside = [{ id: "a" }];
    const other = summarizeLastMove(
      { root: "d:/projects/book-notes", kept: [k("a")], failed: [k("a")] },
      here,
      outside,
    );
    expect(other).toEqual({ kept: [], error: null, allKept: false });
  });

  it("names only panes still outside the root", () => {
    const summary = summarizeLastMove({ root: here, kept: [k("a"), k("gone")], failed: [] }, here, [
      { id: "a" },
    ]);
    expect(summary.kept.map((x) => x.id)).toEqual(["a"]);
    expect(summary.allKept).toBe(true);
  });
});
