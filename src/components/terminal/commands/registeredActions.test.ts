/**
 * The REGISTERED action set, exercised through the real registration hook.
 *
 * Every other spec in this directory tests a pure helper against a fixture
 * registry. That is the right default, and it is also exactly how the three
 * defects below survived review: `safeToReroute`'s `destructive` half had no
 * declarant anywhere in the product (so half the guard was dead code), and
 * `resolveCount` had a floor but no ceiling. A fixture registry cannot see
 * either, because a fixture declares whatever the test needs.
 *
 * So this file registers the ACTUAL 40 actions. `useTerminalCommands` is a
 * hook only in the sense that it calls context hooks and `useCommandAction`;
 * with those mocked it is an ordinary function, which is why this runs under
 * vitest's `environment: "node"` with no React renderer.
 */

import { beforeAll, describe, expect, it, vi } from "vitest";

import { getAll, getById } from "./registry";
import type { CommandAction, CommandResult } from "./types";

vi.mock("@tauri-apps/api/core", () => ({ invoke: async () => ({}) }));
vi.mock("@/lib/instance-storage", () => ({
  instanceStorage: {
    setItem: () => {},
    getItem: () => null,
    setJSON: () => {},
    getJSON: () => null,
  },
}));
vi.mock("@/lib/clipboard", () => ({ writeClipboard: async () => true }));
vi.mock("../terminalHotStore", () => ({ getTerminalHotStore: () => ({ getField: () => ({}) }) }));
vi.mock("../result-card", () => ({
  buildMetricsCardSpec: () => ({ title: "m", sections: [] }),
  buildHistoryCardSpec: () => ({ title: "h", sections: [] }),
}));
vi.mock("../liveClaudeSessions", () => ({
  extractLiveSessions: () => [],
  groupByAccount: () => new Map(),
  sharedSessionIds: () => new Set(),
}));
vi.mock("../contexts", () => ({
  useTerminalSession: () => ({
    tabs: [{ id: "tab-a" }],
    activeId: "tab-a",
    setActiveId: () => {},
    closeTerminal: () => {},
    terminalRefs: { current: new Map() },
    sessionStates: { "tab-a": "needs-input" },
    pageId: "page-1",
    stateTimeAccum: { current: {} },
    // Every field below that a handler READS as pre-state has to be present,
    // or the fixture models a page that cannot exist and the handlers under
    // test derive their verdicts from `undefined`. That is the fixture-drift
    // failure `realRegistry.testkit.ts`'s docstring names as this directory's
    // single structural cause, so it is kept in step deliberately.
    zoneLayout: {
      focusedZone: 0,
      layout: { id: "quad", zones: [{}, {}, {}, {}] },
      layoutId: "quad",
      maximizedZone: null,
      assignments: { 0: "tab-a" },
      focusNextZone: () => ({ changed: true }),
      focusPrevZone: () => ({ changed: true }),
      focusNextNeedsInput: () => true,
      setFocusedZone: () => {},
      toggleMaximize: () => {},
      setLayoutId: () => {},
      assignTabToZone: () => {},
    },
    workflowGen: {
      generatedWorkflow: null,
      planFileName: null,
      handleGenerateFromLatestSession: async () => ({ ok: false, code: "x", message: "x" }),
      handleSaveWorkflow: async () => {},
      handleBuildPlanFromFile: async () => {},
      handleBuildPlanImplementationFromFile: async () => {},
      loadPlanContent: async () => ({ filename: null, chars: 0 }),
      rightPanelMode: null,
      setRightPanelMode: () => {},
      showSidebar: false,
      setShowSidebar: () => {},
    },
    findingsActions: { handleToggleFindings: () => {} },
    analysis: { handleAnalyze: async () => ({ ok: true, panels: 0 }) },
  }),
  useTransitionEffects: () => ({
    autoApprovePatterns: ["armed"],
    setAutoApprovePatterns: () => {},
    autoApproveCount: 0,
    autoRestart: false,
    autoRestartCount: 0,
    setAutoRestart: () => {},
    desktopNotify: false,
    setDesktopNotify: () => {},
    soundEnabled: false,
    toggleSound: () => {},
    autoFocusNeedsInput: false,
    toggleAutoFocus: () => {},
    handleRestartInZone: async () => ({ restarted: false, reason: "not-restartable" }),
  }),
  useUIStateCx: () => ({
    state: { showShortcutsOverlay: false, focusMode: false, selectedZones: new Set<number>() },
    dispatch: () => {},
    toggleFocusMode: () => {},
  }),
  useZoneMetadata: () => ({
    labelsAndTags: {
      allTags: [],
      activeTagFilters: new Set<string>(),
      setActiveTagFilters: () => {},
      zoneLabels: {},
    },
    metrics: { current: {} },
    eventHistory: { current: [] },
  }),
}));
vi.mock("./useCommandAction", async () => {
  const reg = await import("./registry");
  return { useCommandAction: (a: CommandAction) => reg.register(a) };
});

const spawned: number[] = [];

beforeAll(async () => {
  const { useTerminalCommands } = await import("./useTerminalCommands");
  useTerminalCommands({
    spawnPlain: async (n: number) => {
      spawned.push(n);
      return Array.from({ length: n }, (_, i) => `plain-${i}`);
    },
    spawnAi: async (n: number) => {
      spawned.push(n);
      return Array.from({ length: n }, (_, i) => `ai-${i}`);
    },
    accounts: [{ label: "gmail", config_dir: "/cfg/gmail", usage_delta: -0.5 }],
    tenantCandidates: ["2299aaaa-0000-4000-8000-000000000001"],
    // `as never` below means a missing closure is a RUNTIME TypeError, not a
    // compile error — so every closure the context declares is listed here,
    // including `approveAll`, which this file only reaches through the
    // `destructive` declaration today but would crash on the day it does not.
    approveAll: async (tabIds: readonly string[]) => ({
      targeted: tabIds.length,
      delivered: 0,
      deliveries: tabIds.map((tabId) => ({ tabId, route: "by-id", delivered: false })),
    }),
    sortZones: () => ({ moved: 0, total: 1 }),
    exportAll: async () => ({ exported: 0, cancelled: false }),
    openDocFinder: () => ({ changed: true }),
    openPromptModal: () => ({ changed: true }),
    showCard: () => {},
  } as never);
});

const run = (id: string, args: Record<string, unknown>): Promise<CommandResult> =>
  getById(id)!.handler(args, { source: "test" }) as Promise<CommandResult>;

const err = (r: CommandResult): string => (r.ok ? "OK" : `${r.code}: ${r.message ?? ""}`);

describe("registered actions — the cost/destruction declarations are not empty", () => {
  it("registers the whole set", () => {
    expect(getAll().length).toBe(40);
  });

  /**
   * `rank.ts::safeToReroute` is `!costly && !destructive`. `destructive`
   * appeared on NO action, so the second half was unreachable and the guard
   * was one flag with one declarant. These two lists are what make it a
   * guard; a new action that spends or destroys belongs in one of them.
   */
  it("declares every SPENDING action costly", () => {
    expect(
      getAll()
        .filter((a) => a.costly)
        .map((a) => a.id)
        .sort(),
    ).toEqual([
      "terminal.analyze",
      "terminal.generate-workflow",
      "terminal.orchestrate",
      "terminal.plan-implement",
      "terminal.spawn",
      "terminal.spawn-ai",
      "terminal.spawn-with",
    ]);
  });

  it("declares every DESTROYING action destructive", () => {
    expect(
      getAll()
        .filter((a) => a.destructive)
        .map((a) => a.id)
        .sort(),
    ).toEqual(["terminal.approve-all", "terminal.close", "terminal.restart"]);
  });

  it("leaves the free, local, reversible actions unflagged", () => {
    for (const id of [
      "terminal.focus",
      "terminal.toggle-focus-mode",
      "terminal.maximize",
      "terminal.layout",
      "terminal.swap",
      "terminal.tag-toggle",
      "terminal.plan-verify",
    ]) {
      const a = getById(id)!;
      expect([id, a.costly ?? false, a.destructive ?? false]).toEqual([id, false, false]);
    }
  });
});

describe("registered actions — /spawn* is bounded at both ends", () => {
  /**
   * D12. The floor was checked; there was no ceiling, so `/spawn 1000`
   * rendered `✓`, created a thousand PTYs and wedged the page badly enough
   * to need a reboot — a verdict the operator never got to read.
   */
  const SPAWNERS: Array<[string, Record<string, unknown>]> = [
    ["terminal.spawn", {}],
    ["terminal.spawn-ai", { account: "gmail" }],
    ["terminal.spawn-with", { command: "ls" }],
  ];

  it.each(SPAWNERS)("%s refuses a count past the ceiling", async (id, extra) => {
    spawned.length = 0;
    expect(err(await run(id, { count: 1000, ...extra }))).toBe(
      "invalid-count: count must be <= 24 (asked for 1000)",
    );
    expect(err(await run(id, { count: 25, ...extra }))).toBe(
      "invalid-count: count must be <= 24 (asked for 25)",
    );
    // The refusal is BEFORE the spawn closure, not after it.
    expect(spawned).toEqual([]);
  });

  it.each(SPAWNERS)("%s still refuses a count below the floor", async (id, extra) => {
    expect(err(await run(id, { count: 0, ...extra }))).toBe("invalid-count: count must be >= 1");
  });

  it.each(SPAWNERS)("%s accepts the ceiling itself", async (id, extra) => {
    spawned.length = 0;
    expect((await run(id, { count: 24, ...extra })).ok).toBe(true);
    expect(spawned).toEqual([24]);
  });
});

describe("registered actions — /auto-approve names its missing argument", () => {
  /**
   * D13. A bare `/auto-approve` reported `unknown action ""`, which
   * describes an argument supplied as empty. Nothing was supplied.
   */
  it("reports an ABSENT sub-command as required", async () => {
    expect(err(await run("terminal.auto-approve", {}))).toBe(
      "invalid-args: action is required (add, remove, list, clear)",
    );
  });

  it("reads a supplied-but-empty sub-command the same way", async () => {
    for (const supplied of ["", " ", "  "]) {
      expect(err(await run("terminal.auto-approve", { action: supplied }))).toBe(
        "invalid-args: action is required (add, remove, list, clear)",
      );
    }
  });

  it("still names an action it does not recognise", async () => {
    expect(err(await run("terminal.auto-approve", { action: "bogus" }))).toBe(
      'invalid-args: unknown action "bogus"',
    );
  });

  it("leaves the real sub-commands working", async () => {
    expect((await run("terminal.auto-approve", { action: "list" })).ok).toBe(true);
    expect((await run("terminal.auto-approve", { action: "clear" })).ok).toBe(true);
    expect((await run("terminal.auto-approve", { action: "add", pattern: "y/n" })).ok).toBe(true);
    expect(err(await run("terminal.auto-approve", { action: "add", pattern: "" }))).toBe(
      "invalid-args: pattern required for add",
    );
  });
});
