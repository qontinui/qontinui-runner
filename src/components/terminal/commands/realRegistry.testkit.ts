/**
 * The REAL action registry, loaded once per test file, with every context
 * closure replaced by a RECORDING stub.
 *
 * Why this file exists
 * --------------------
 * Nine rounds of `/manual-test-loop` against the CommandBar found 64 defects
 * in this pipeline, and the same failure recurred eight times: a fix passed
 * review because the defective SHAPE lived somewhere the fix did not reach.
 * Three of those recurrences were inside the very fix meant to close the
 * class. In nearly every round an agent built a large differential harness
 * from scratch, used it to catch a regression its own reasoning had missed,
 * and then threw it away. Nothing in the repository could see the shape.
 *
 * The single structural cause is that every spec in this directory tests a
 * pure helper against a FIXTURE registry — a registry that declares whatever
 * the test needs. A fixture cannot see:
 *
 *   - a pattern the product actually ships that binds args differently from
 *     the slash route (`patterns.ts` vs `parse.ts` never met in a test),
 *   - a `costly`/`destructive` declaration that is missing on a real action,
 *   - a handler that answers `✓` for something that did not happen.
 *
 * So this module registers the ACTUAL product actions the way
 * `registeredActions.test.ts` does, and adds the two things that file lacks:
 * a reusable loader (so more than one spec can have the real registry) and
 * an EFFECTS LEDGER (so a handler's verdict can be compared against what it
 * actually did).
 *
 * `useTerminalCommands` is a "hook" only in that it calls context hooks and
 * `useCommandAction`; it uses no React state hooks at all (verified: no
 * `useMemo` / `useCallback` / `useEffect` / `useRef` / `useState` anywhere in
 * it or in `orchestrateCommand.ts`). With the context hooks mocked it is an
 * ordinary function, which is why this runs under vitest's
 * `environment: "node"` with no React renderer and no jsdom.
 *
 * Mocking style: `vi.doMock` (NOT `vi.mock`) so the mocks can live in this
 * shared module instead of being copy-pasted into every spec's top level.
 * `doMock` is not hoisted, so it only affects the DYNAMIC import below — a
 * spec must therefore never statically import `useTerminalCommands`, and none
 * of them do.
 */

import { vi } from "vitest";

import type { CommandAction } from "./types";

/** One recorded call into a stubbed context closure or Tauri command. */
export interface EffectCall {
  /** Dotted name of the closure, e.g. `"ctx.spawnAi"` or `"invoke"`. */
  name: string;
  /** The arguments it was called with, as passed. */
  args: unknown[];
  /**
   * False when the closure returns `undefined` — i.e. it hands the handler NO
   * evidence that anything happened.
   *
   * This is the field `handlers.test.ts` reads. A handler that answers `ok`
   * having called only evidence-free closures cannot have DERIVED its verdict;
   * it asserted one. That is the ~34-handler shape the later phase has to fix,
   * and pinning it here is what makes that fix show up as a diff instead of a
   * silent semantic shift.
   */
  evidence: boolean;
}

/** What a loaded real registry gives a spec. */
export interface RealRegistryHarness {
  /** Every registered action, in registration order. */
  actions: readonly CommandAction[];
  /** Look up by id; throws when unknown so a typo is not a silent skip. */
  byId(id: string): CommandAction;
  /** The effects ledger — every stubbed closure call since the last reset. */
  calls: EffectCall[];
  /** Names only, for terse assertions. */
  callNames(): string[];
  /** Clear the ledger. Call between handler invocations. */
  reset(): void;
}

let cached: Promise<RealRegistryHarness> | null = null;

/**
 * Load (once per test file) the real registry with recording stubs.
 *
 * Everything the product's handlers close over is stubbed here. The stubs
 * are deliberately PERMISSIVE — they succeed — because the question these
 * specs ask is "what does the pipeline bind and report", not "what does the
 * Tauri backend do". Where a handler's verdict depends on a stub's return
 * shape, the shape is pinned here with a comment.
 */
export function loadRealRegistry(): Promise<RealRegistryHarness> {
  if (cached) return cached;
  cached = build();
  return cached;
}

async function build(): Promise<RealRegistryHarness> {
  const calls: EffectCall[] = [];
  const rec =
    <T>(name: string, result: T) =>
    (...args: unknown[]): T => {
      calls.push({ name, args, evidence: result !== undefined });
      return result;
    };
  const recAsync =
    <T>(name: string, result: T) =>
    async (...args: unknown[]): Promise<T> => {
      calls.push({ name, args, evidence: result !== undefined });
      return result;
    };

  // ── Module stubs ───────────────────────────────────────────────────
  // `invoke` answers the two commands the registry actually calls
  // (`terminal_claude_session_list_live`, `start_orchestration_run`) and a
  // generic empty object for anything else, so a newly added invoke does not
  // crash the suite — it shows up in the ledger instead.
  vi.doMock("@tauri-apps/api/core", () => ({
    invoke: async (cmd: string, payload?: unknown) => {
      calls.push({ name: "invoke", args: [cmd, payload], evidence: true });
      if (cmd === "terminal_claude_session_list_live") {
        return { success: true, data: { sessions: [] } };
      }
      if (cmd === "start_orchestration_run") {
        return { id: "run-1", status: "running" };
      }
      return {};
    },
  }));
  vi.doMock("@/lib/instance-storage", () => ({
    instanceStorage: {
      setItem: rec("instanceStorage.setItem", undefined),
      getItem: rec("instanceStorage.getItem", null),
      setJSON: rec("instanceStorage.setJSON", undefined),
      getJSON: rec("instanceStorage.getJSON", null),
    },
  }));
  vi.doMock("@/lib/clipboard", () => ({
    writeClipboard: recAsync("writeClipboard", true),
  }));
  vi.doMock("../terminalHotStore", () => ({
    getTerminalHotStore: () => ({ getField: () => ({}) }),
  }));
  vi.doMock("../result-card", () => ({
    buildMetricsCardSpec: () => ({ title: "m", sections: [] }),
    buildHistoryCardSpec: () => ({ title: "h", sections: [] }),
  }));
  vi.doMock("../liveClaudeSessions", () => ({
    extractLiveSessions: () => [],
    groupByAccount: () => new Map(),
    sharedSessionIds: () => new Set(),
  }));
  vi.doMock("../contexts", () => ({
    useTerminalSession: () => ({
      tabs: [{ id: "tab-a" }, { id: "tab-b" }],
      activeId: "tab-a",
      setActiveId: rec("session.setActiveId", undefined),
      closeTerminal: rec("session.closeTerminal", undefined),
      terminalRefs: { current: new Map() },
      sessionStates: { "tab-a": "needs-input", "tab-b": "idle" },
      pageId: "page-1",
      stateTimeAccum: { current: {} },
      zoneLayout: {
        focusedZone: 0,
        layout: { id: "quad", zones: [{}, {}, {}, {}] },
        assignments: { 0: "tab-a", 1: "tab-b" },
        focusNextZone: rec("zone.focusNextZone", undefined),
        focusPrevZone: rec("zone.focusPrevZone", undefined),
        // `true` = "a needs-input zone was found and focused". The `false`
        // arm is what `/focus needs-input` reports as a failure, so the
        // truthy stub pins the SUCCESS path; specs that want the other arm
        // assert on the ledger instead of on this return.
        focusNextNeedsInput: rec("zone.focusNextNeedsInput", true),
        setFocusedZone: rec("zone.setFocusedZone", undefined),
        toggleMaximize: rec("zone.toggleMaximize", undefined),
        setLayoutId: rec("zone.setLayoutId", undefined),
        assignTabToZone: rec("zone.assignTabToZone", undefined),
      },
      workflowGen: {
        generatedWorkflow: null,
        planFileName: null,
        handleGenerateFromLatestSession: recAsync("workflow.generate", {
          ok: false,
          code: "no-session",
          message: "no session",
        }),
        handleSaveWorkflow: recAsync("workflow.save", undefined),
        handleBuildPlanFromFile: recAsync("workflow.buildPlan", undefined),
        handleBuildPlanImplementationFromFile: recAsync("workflow.buildPlanImpl", undefined),
        loadPlanContent: recAsync("workflow.loadPlanContent", ""),
        setRightPanelMode: rec("workflow.setRightPanelMode", undefined),
        setShowSidebar: rec("workflow.setShowSidebar", undefined),
      },
      findingsActions: { handleToggleFindings: rec("findings.toggle", undefined) },
      analysis: { handleAnalyze: rec("analysis.handleAnalyze", undefined) },
    }),
    useTransitionEffects: () => ({
      autoApprovePatterns: ["armed"],
      setAutoApprovePatterns: rec("effects.setAutoApprovePatterns", undefined),
      autoApproveCount: 0,
      autoRestartCount: 0,
      setAutoRestart: rec("effects.setAutoRestart", undefined),
      setDesktopNotify: rec("effects.setDesktopNotify", undefined),
      soundEnabled: false,
      toggleSound: rec("effects.toggleSound", undefined),
      toggleAutoFocus: rec("effects.toggleAutoFocus", undefined),
      handleRestartInZone: rec("effects.handleRestartInZone", undefined),
    }),
    useUIStateCx: () => ({
      dispatch: rec("ui.dispatch", undefined),
      toggleFocusMode: rec("ui.toggleFocusMode", undefined),
    }),
    useZoneMetadata: () => ({
      labelsAndTags: {
        allTags: ["alpha"],
        activeTagFilters: new Set<string>(),
        setActiveTagFilters: rec("tags.setActiveTagFilters", undefined),
        zoneLabels: {},
      },
      metrics: { current: {} },
      eventHistory: { current: [] },
    }),
  }));
  vi.doMock("./useCommandAction", async () => {
    const reg = await import("./registry");
    return { useCommandAction: (a: CommandAction) => reg.register(a) };
  });

  const { useTerminalCommands } = await import("./useTerminalCommands");
  const registry = await import("./registry");

  // `useTerminalCommands` is a hook by NAME only: it calls no React state
  // hooks at all (see the module docstring), and here every context hook it
  // does call is mocked, so it is an ordinary function. Calling it is the
  // entire purpose of this file — it is what registers the real action set.
  // eslint-disable-next-line react-hooks/rules-of-hooks
  useTerminalCommands({
    spawnPlain: async (...args: unknown[]) => {
      calls.push({ name: "ctx.spawnPlain", args, evidence: true });
      const n = typeof args[0] === "number" ? args[0] : 1;
      return Array.from({ length: n }, (_, i) => `plain-${i}`);
    },
    spawnAi: async (...args: unknown[]) => {
      calls.push({ name: "ctx.spawnAi", args, evidence: true });
      const n = typeof args[0] === "number" ? args[0] : 1;
      return Array.from({ length: n }, (_, i) => `ai-${i}`);
    },
    accounts: [
      { label: "gmail", config_dir: "/cfg/gmail", usage_delta: -0.5 },
      { label: "hotmail", config_dir: "/cfg/hotmail", usage_delta: -0.9 },
    ],
    tenantCandidates: ["2299aaaa-0000-4000-8000-000000000001", "acme"],
    sortZones: rec("ctx.sortZones", undefined),
    exportAll: rec("ctx.exportAll", undefined),
    openDocFinder: rec("ctx.openDocFinder", undefined),
    openPromptModal: rec("ctx.openPromptModal", undefined),
    showCard: rec("ctx.showCard", undefined),
  } as never);

  const actions = registry.getAll();
  return {
    actions,
    byId(id: string): CommandAction {
      const found = registry.getById(id);
      if (!found) throw new Error(`no such action id: ${id}`);
      return found;
    },
    calls,
    callNames: () => calls.map((c) => c.name),
    reset: () => {
      calls.length = 0;
    },
  };
}
