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
 * ## A stub pinned to ONE arm is a fixture that lies about its coverage
 *
 * The tenth round found the ninth recurrence of the class above, and it was
 * INSIDE this file. `handleRestartInZone` was hard-coded to
 * `{restarted: false, reason: "not-restartable"}` — the failure arm — with a
 * comment justifying the choice. So `RestartOutcome`'s SUCCESS arm,
 * `{restarted: true, tabId, retiredTabId}`, never once reached `countOf` in
 * 91,784 corpus inputs, and `countOf`'s field vocabulary did not contain a
 * single one of its fields. `/restart` reported a fully successful restart as
 * a red `restarted 0 of 1 session`, and every test in this directory was green.
 *
 * The harness did not merely miss it: it made the corpus look like it COVERED
 * `/restart`, because there is a `/restart` row in both goldens. A one-armed
 * stub is worse than no stub, for the same reason a fixture registry is worse
 * than none — it converts a blind spot into a green check-mark.
 *
 * So every stub whose RETURN a handler reads is now an ARM, declared in
 * {@link EffectArms}, defaulted in {@link DEFAULT_ARMS}, and switchable at
 * runtime through {@link RealRegistryHarness.setArms}. {@link ARM_VARIANTS}
 * enumerates the alternates, `__golden__/arms-golden.txt` characterizes what
 * each one does to every handler's rendered verdict, and
 * `handlers.test.ts::"every declared arm is reachable"` fails when an arm
 * changes nothing anywhere — which is what a dead or mis-shaped arm looks like.
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

/**
 * The OUTCOME ARM every stubbed effect is currently answering with.
 *
 * One field per stub whose return value a handler READS to derive its verdict.
 * A stub whose return nobody reads (`session.setActiveId`, `ui.dispatch`) is
 * not here — it has no arms, only a ledger entry.
 *
 * Every field is a closed union so a new arm is a compile error at
 * {@link ARM_VARIANTS} rather than an untested branch.
 */
export interface EffectArms {
  /** `transitionEffects.handleRestartInZone` — `RestartOutcome`'s three exits. */
  restart: "not-restartable" | "spawn-failed" | "restarted";
  /** `zoneLayout.focusNextZone` / `focusPrevZone` — `{changed}`. */
  focusMove: "moved" | "blocked";
  /** `zoneLayout.focusNextNeedsInput` — found one, or there was none. */
  focusNeedsInput: "found" | "none";
  /**
   * Whether the waiting panes have MOUNTED HANDLES.
   *
   * Not a stub return: it populates the `terminalRefs` map that both the
   * session mock and the `ctx.approveAll` stub read, so the delivery count
   * stays derived from the same state the product derives it from.
   */
  approveDelivery: "none" | "all";
  /** `ctx.sortZones` — `{moved, total}`. */
  sortZones: "none" | "moved";
  /** `ctx.exportAll` — `{exported, cancelled}`. */
  exportAll: "exported" | "cancelled";
  /** `ctx.openDocFinder` / `ctx.openPromptModal` — `{changed}`. */
  modalOpen: "opened" | "already-open";
  /** `analysis.handleAnalyze` — panels produced, or a failure. */
  analyze: "panels" | "failed";
  /** `workflowGen.handleGenerateFromLatestSession`. */
  generateWorkflow: "no-session" | "generated";
  /** `workflowGen.loadPlanContent` — `{filename, chars}`. */
  planContent: "none" | "loaded";
  /** `writeClipboard`. */
  clipboard: "written" | "failed";
  /** `invoke("terminal_claude_session_list_live")`. */
  liveSessions: "none" | "two";
  /**
   * How much the RESULT CARDS have in them.
   *
   * `/metrics` and `/history` derive their verdict from the spec they built,
   * so an always-empty event history could not tell a truthful count from a
   * structurally-zero one. It did not: `/history` reported `showed 0 event`
   * under BOTH, because its spec carries a React body and no `sections`.
   */
  cards: "empty" | "populated";
  /** `ctx.spawnPlain` / `ctx.spawnAi` — all the ids asked for, or one short. */
  spawn: "full" | "short";
  /**
   * The fixture's SESSION STATES — a prerequisite arm, not an effect return.
   *
   * `/restart` refuses any zone whose session is not `completed` or `error`,
   * and the canonical fixture's zone 1 holds a `needs-input` session. So under
   * the default states the handler never calls `handleRestartInZone` at all:
   * the restart effect was unreachable, not merely one-armed. An arm sitting
   * behind a closed state gate is not covered by anything, which is why the
   * two `restart=` variants below carry this arm with them.
   */
  sessionStates: "waiting" | "errored";
}

/**
 * The canonical fixture: the arm each stub answered with BEFORE arms existed.
 *
 * Preserved exactly so the committed goldens do not move when the mechanism
 * lands. Where a default looks odd (`restart: "not-restartable"`, `sortZones:
 * "none"`) it is the historical value, and the alternate arm is what was
 * missing — not the default that was wrong.
 */
export const DEFAULT_ARMS: EffectArms = {
  restart: "not-restartable",
  focusMove: "moved",
  focusNeedsInput: "found",
  approveDelivery: "none",
  sortZones: "none",
  exportAll: "exported",
  modalOpen: "opened",
  analyze: "panels",
  generateWorkflow: "no-session",
  planContent: "none",
  clipboard: "written",
  liveSessions: "none",
  cards: "empty",
  spawn: "full",
  sessionStates: "waiting",
};

/**
 * Every arm that is NOT the default, one variant each.
 *
 * Deliberately one-field-at-a-time: a variant that flipped several arms could
 * not attribute a changed verdict to the arm that caused it, and attribution is
 * the whole point — `arms-golden.txt` reads as "under THIS arm, THIS handler
 * says THIS".
 */
export const ARM_VARIANTS: ReadonlyArray<{ name: string; arms: Partial<EffectArms> }> = [
  { name: "sessionStates=errored", arms: { sessionStates: "errored" } },
  // These two carry `sessionStates` because `/restart`'s own state gate would
  // otherwise refuse before the effect is called — see `EffectArms.sessionStates`.
  // "One field at a time" means one CONCERN at a time; a prerequisite the arm
  // cannot be reached without is part of the arm.
  { name: "restart=restarted", arms: { sessionStates: "errored", restart: "restarted" } },
  { name: "restart=spawn-failed", arms: { sessionStates: "errored", restart: "spawn-failed" } },
  { name: "focusMove=blocked", arms: { focusMove: "blocked" } },
  { name: "focusNeedsInput=none", arms: { focusNeedsInput: "none" } },
  { name: "approveDelivery=all", arms: { approveDelivery: "all" } },
  { name: "sortZones=moved", arms: { sortZones: "moved" } },
  { name: "exportAll=cancelled", arms: { exportAll: "cancelled" } },
  { name: "modalOpen=already-open", arms: { modalOpen: "already-open" } },
  { name: "analyze=failed", arms: { analyze: "failed" } },
  { name: "generateWorkflow=generated", arms: { generateWorkflow: "generated" } },
  { name: "planContent=loaded", arms: { planContent: "loaded" } },
  // `liveSessions` too: `/copy-names` is the only clipboard writer, and it
  // fails with `no-sessions` before reaching the clipboard when the registry
  // is empty. Same prerequisite shape as the restart arms above.
  { name: "clipboard=failed", arms: { liveSessions: "two", clipboard: "failed" } },
  { name: "liveSessions=two", arms: { liveSessions: "two" } },
  { name: "cards=populated", arms: { cards: "populated" } },
  { name: "spawn=short", arms: { spawn: "short" } },
];

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
  /** The arms currently in force. Read-only view; mutate via {@link setArms}. */
  arms: Readonly<EffectArms>;
  /** Switch one or more stubs onto a different outcome arm. */
  setArms(next: Partial<EffectArms>): void;
  /** Restore {@link DEFAULT_ARMS}. Call in a `finally`, or the next spec inherits. */
  resetArms(): void;
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
  const arms: EffectArms = { ...DEFAULT_ARMS };
  /**
   * The mounted-pane registry, hoisted so `useTerminalSession`'s mock and the
   * `ctx.approveAll` stub below read the SAME map.
   *
   * Empty under the DEFAULT arm, and that is the fixture's most load-bearing
   * property: it is the state in which `/approve-all` used to report three
   * approvals having written nothing at all. The `approveDelivery` arm
   * populates it, so the delivered path is exercised too — an all-refused
   * fixture cannot characterise a successful delivery any more than a
   * one-armed restart stub can characterise a successful restart.
   */
  const terminalRefs = new Map<string, { current: unknown }>();
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
  /**
   * The ARMED recorders — identical to `rec` / `recAsync` except that the
   * return value is produced at CALL time from a thunk, so `setArms` can move
   * it after the registry has been built.
   *
   * The registry is built once per test file (`loadRealRegistry` is cached),
   * and every handler closes over these functions at registration time. A stub
   * whose value were captured at build time — which is what the previous
   * literal returns were — could therefore only ever answer one arm for the
   * whole file. That is not a stylistic difference; it is the mechanism that
   * hid `RestartOutcome`'s success arm from 91,784 inputs.
   */
  const recArm =
    <T>(name: string, produce: () => T) =>
    (...args: unknown[]): T => {
      const result = produce();
      calls.push({ name, args, evidence: result !== undefined });
      return result;
    };
  const recArmAsync =
    <T>(name: string, produce: () => T) =>
    async (...args: unknown[]): Promise<T> => {
      const result = produce();
      calls.push({ name, args, evidence: result !== undefined });
      return result;
    };
  /**
   * Keep `terminalRefs` in step with the `approveDelivery` arm.
   *
   * The map is the state BOTH the session mock and the `ctx.approveAll` stub
   * read, so driving the arm through it (rather than through a delivered-count
   * literal) keeps the property that made the original stub honest: the count
   * is derived from mounted handles, never asserted.
   */
  /**
   * MUTABLE fixture state, hoisted so an arm can move it AFTER registration.
   *
   * `useTerminalCommands` runs exactly once (this module is cached per test
   * file) and its handlers close over whatever the context mocks returned at
   * that moment. A stub that returns a fresh literal per call can be armed with
   * a thunk; a value the hook DESTRUCTURES — `eventHistory`, `sessionStates`,
   * the metrics ref — cannot, because the handler holds the object, not the
   * getter. So those are single objects created here and mutated in place.
   */
  const sessionStates: Record<string, string> = {};
  const metricsRef: { current: Record<string, number> } = { current: {} };
  const eventHistory: unknown[] = [];

  /** Re-derive the mutable fixture state from the arms currently in force. */
  const syncFixtures = (): void => {
    for (const k of Object.keys(sessionStates)) delete sessionStates[k];
    Object.assign(
      sessionStates,
      arms.sessionStates === "errored"
        ? { "tab-a": "error", "tab-b": "idle" }
        : { "tab-a": "needs-input", "tab-b": "idle" },
    );
    metricsRef.current =
      arms.cards === "empty"
        ? {}
        : { totalApprovals: 4, totalRejections: 1, totalBroadcasts: 2, sessionsCreated: 3 };
    eventHistory.length = 0;
    if (arms.cards === "populated") {
      eventHistory.push(
        { time: 1_700_000_000_000, type: "working", session: "tab-a", zone: 0, color: "#7aa2f7" },
        { time: 1_700_000_001_000, type: "waiting", session: "tab-b", zone: 1, color: "#e0af68" },
        { time: 1_700_000_002_000, type: "done", session: "tab-a", zone: 0, color: "#9ece6a" },
      );
    }
  };

  /** The `liveSessions` arm's rows, in the Rust command's own camelCase shape. */
  const liveSessionRows = (): unknown[] =>
    arms.liveSessions === "none"
      ? []
      : [
          { sessionId: "sess-1", name: "alpha", pid: 101, account: { label: "gmail" } },
          { sessionId: "sess-2", name: "beta", pid: 102, account: { label: "hotmail" } },
        ];

  const syncTerminalRefs = (): void => {
    terminalRefs.clear();
    if (arms.approveDelivery === "all") {
      for (const id of ["tab-a", "tab-b"]) terminalRefs.set(id, { current: { mounted: true } });
    }
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
        return { success: true, data: { sessions: liveSessionRows() } };
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
    writeClipboard: recArmAsync("writeClipboard", () => arms.clipboard === "written"),
  }));
  // The hot store has no arms: `/metrics` reads it for extra colour
  // (`lastOutputLines`, `stateDurations`), never for its count. Its rows come
  // from `metrics` / `sessionStates` / `tabs`, which the `cards` arm moves.
  vi.doMock("../terminalHotStore", () => ({
    getTerminalHotStore: () => ({ getField: () => ({}) }),
  }));
  // `../result-card` is deliberately NOT mocked any more.
  //
  // It used to be stubbed as `{title: "h", sections: []}` for BOTH builders —
  // a shape the real `buildHistoryCardSpec` has never returned. The real one
  // returns `title`/`subtitle`/`body` with no `sections` at all, and
  // `countCardRows` summed `sections`, so `/history` was structurally
  // incapable of reporting a non-zero count. The mock hid that twice over: it
  // gave the two builders the SAME shape, and it made the zero look like an
  // empty fixture rather than a broken read. The builders are pure functions
  // over plain data and run fine under `environment: "node"`
  // (`result-card/builders.test.tsx` already does exactly that), so the
  // handlers now build real specs from the `cards` arm's data.
  vi.doMock("../liveClaudeSessions", () => ({
    extractLiveSessions: () => liveSessionRows(),
    groupByAccount: () => new Map(),
    sharedSessionIds: () => new Set(),
  }));
  vi.doMock("../contexts", () => ({
    useTerminalSession: () => ({
      tabs: [{ id: "tab-a" }, { id: "tab-b" }],
      activeId: "tab-a",
      setActiveId: rec("session.setActiveId", undefined),
      closeTerminal: rec("session.closeTerminal", undefined),
      terminalRefs: { current: terminalRefs },
      sessionStates,
      pageId: "page-1",
      stateTimeAccum: { current: {} },
      zoneLayout: {
        focusedZone: 0,
        layout: { id: "quad", zones: [{}, {}, {}, {}] },
        // `layoutId` is the STATE `setLayoutId` writes; `layout` is derived
        // from it. `/layout` reads the former as its pre-state, so the two
        // must agree here or the stub would model a page that cannot exist.
        layoutId: "quad",
        // null = nothing maximized, which is what `/maximize` reads to decide
        // whether it is maximizing or restoring.
        maximizedZone: null,
        assignments: { 0: "tab-a", 1: "tab-b" },
        // `{changed}` = "focus actually moved". The cap these two apply is
        // `min(zones, tabs)`, which the handler cannot see — so the stub has
        // to answer in the same shape the product does, or the fixture would
        // model a `focusNextZone` that reports nothing and the handler's
        // verdict would go back to being asserted.
        focusNextZone: recArm("zone.focusNextZone", () => ({
          changed: arms.focusMove === "moved",
        })),
        focusPrevZone: recArm("zone.focusPrevZone", () => ({
          changed: arms.focusMove === "moved",
        })),
        // `true` = "a needs-input zone was found and focused". The `false`
        // arm is what `/focus needs-input` reports as a failure, so the
        // truthy stub pins the SUCCESS path; specs that want the other arm
        // assert on the ledger instead of on this return.
        focusNextNeedsInput: recArm(
          "zone.focusNextNeedsInput",
          () => arms.focusNeedsInput === "found",
        ),
        setFocusedZone: rec("zone.setFocusedZone", undefined),
        toggleMaximize: rec("zone.toggleMaximize", undefined),
        setLayoutId: rec("zone.setLayoutId", undefined),
        assignTabToZone: rec("zone.assignTabToZone", undefined),
      },
      workflowGen: {
        generatedWorkflow: null,
        planFileName: null,
        handleGenerateFromLatestSession: recArmAsync("workflow.generate", () =>
          arms.generateWorkflow === "generated"
            ? { ok: true, sessionId: "sess-1" }
            : { ok: false, code: "no-session", message: "no session" },
        ),
        handleSaveWorkflow: recAsync("workflow.save", undefined),
        handleBuildPlanFromFile: recAsync("workflow.buildPlan", undefined),
        handleBuildPlanImplementationFromFile: recAsync("workflow.buildPlanImpl", undefined),
        // Was stubbed as `""` while the real function returned `void` — the
        // stub, not the product, is what put `evidence=true` on
        // `/plan-refresh`'s golden row. Both now report the same shape: what
        // the reload actually found. `filename: null` is the honest answer
        // for a fixture with no workspace plan file.
        loadPlanContent: recArmAsync("workflow.loadPlanContent", () =>
          arms.planContent === "loaded"
            ? { filename: "PLAN.md", chars: 1200 }
            : { filename: null, chars: 0 },
        ),
        rightPanelMode: null,
        setRightPanelMode: rec("workflow.setRightPanelMode", undefined),
        showSidebar: false,
        setShowSidebar: rec("workflow.setShowSidebar", undefined),
      },
      findingsActions: { handleToggleFindings: rec("findings.toggle", undefined) },
      // `/analyze` now awaits this and reads the outcome; a metered call that
      // came back with no panels is no longer a `✓`.
      analysis: {
        handleAnalyze: recArmAsync("analysis.handleAnalyze", () =>
          arms.analyze === "panels"
            ? { ok: true, panels: 2 }
            : { ok: false, panels: 0, message: "analysis failed" },
        ),
      },
    }),
    // Every `*Enabled` / `auto*` field below is now READ by a handler as its
    // observed pre-state, not just written to. They are pinned `false`/`[…]`
    // so the toggles all run their "was off, turning on" arm, which is the
    // arm the golden table characterizes.
    useTransitionEffects: () => ({
      autoApprovePatterns: ["armed"],
      setAutoApprovePatterns: rec("effects.setAutoApprovePatterns", undefined),
      autoApproveCount: 0,
      autoRestart: false,
      autoRestartCount: 0,
      setAutoRestart: rec("effects.setAutoRestart", undefined),
      desktopNotify: false,
      setDesktopNotify: rec("effects.setDesktopNotify", undefined),
      soundEnabled: false,
      toggleSound: rec("effects.toggleSound", undefined),
      autoFocusNeedsInput: false,
      toggleAutoFocus: rec("effects.toggleAutoFocus", undefined),
      /**
       * `/restart` AWAITS this and reads the outcome — all THREE arms of it.
       *
       * This stub was hard-coded to the `not-restartable` arm, with a comment
       * arguing that the canonical fixture's zone-1 session is `needs-input`
       * and so a success "would model a transition the fixture cannot reach".
       * The argument was about the FIXTURE's zone assignment; the consequence
       * was that `RestartOutcome`'s success object never reached `countOf` in
       * any test, and `countOf` could not read it. `/restart` rendered a
       * completed restart as red. The handler's own state gate is what decides
       * whether it calls this at all, so answering the success arm here models
       * a page where the gate passed — which is most pages.
       */
      handleRestartInZone: recArmAsync("effects.handleRestartInZone", () =>
        arms.restart === "restarted"
          ? { restarted: true, tabId: "tab-restarted", retiredTabId: "tab-a" }
          : { restarted: false, reason: arms.restart },
      ),
    }),
    useUIStateCx: () => ({
      // `state` is read by `/shortcuts` and `/focus-mode` as pre-state.
      state: { showShortcutsOverlay: false, focusMode: false, selectedZones: new Set<number>() },
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
      // `metrics` IS a ref in the product (`metricsRef.current`); the handler
      // reads `.current` at command time, so the arm swaps the ref's contents.
      metrics: metricsRef,
      /**
       * `eventHistory` is a plain ARRAY in the product
       * (`useEventHistory.ts:19` — `eventHistory: HistoryEntry[]`), and this
       * stub answered `{current: []}`. `/history` does
       * `buildHistoryCardSpec(eventHistory ?? [])`, so it was handing the
       * builder an object where an array belongs — a shape the product cannot
       * produce, and one the real builder would have thrown on. The mocked
       * `../result-card` swallowed it: the fixture modelled a page that does
       * not exist, and nothing could tell.
       */
      eventHistory,
    }),
  }));
  vi.doMock("./useCommandAction", async () => {
    const reg = await import("./registry");
    return { useCommandAction: (a: CommandAction) => reg.register(a) };
  });

  syncFixtures();

  const { useTerminalCommands } = await import("./useTerminalCommands");
  const registry = await import("./registry");

  // `useTerminalCommands` is a hook by NAME only: it calls no React state
  // hooks at all (see the module docstring), and here every context hook it
  // does call is mocked, so it is an ordinary function. Calling it is the
  // entire purpose of this file — it is what registers the real action set.
  // eslint-disable-next-line react-hooks/rules-of-hooks
  useTerminalCommands({
    // The `spawn` arm's `short` value returns ONE FEWER id than asked for —
    // the partial-delivery shape `spawnVerdict` exists to catch, and the one
    // that used to render `✓` for two terminals that were never created.
    spawnPlain: async (...args: unknown[]) => {
      calls.push({ name: "ctx.spawnPlain", args, evidence: true });
      const n = typeof args[0] === "number" ? args[0] : 1;
      const produced = arms.spawn === "short" ? Math.max(0, n - 1) : n;
      return Array.from({ length: produced }, (_, i) => `plain-${i}`);
    },
    spawnAi: async (...args: unknown[]) => {
      calls.push({ name: "ctx.spawnAi", args, evidence: true });
      const n = typeof args[0] === "number" ? args[0] : 1;
      const produced = arms.spawn === "short" ? Math.max(0, n - 1) : n;
      return Array.from({ length: produced }, (_, i) => `ai-${i}`);
    },
    accounts: [
      // Both under pace, so `best` ranks on `expected_utilization`
      // DESCENDING (use-it-or-lose-it). `expected_utilization` is required
      // for that: with `usage_delta` alone these land in the Unknown tier
      // and stop testing the ranked arm at all.
      {
        label: "gmail",
        config_dir: "/cfg/gmail",
        usage_delta: -0.5,
        expected_utilization: 0.7,
      },
      {
        label: "hotmail",
        config_dir: "/cfg/hotmail",
        usage_delta: -0.9,
        expected_utilization: 0.3,
      },
    ],
    tenantCandidates: ["2299aaaa-0000-4000-8000-000000000001", "acme"],
    /**
     * `/approve-all`'s delivery path — the stub that makes the count
     * falsifiable.
     *
     * It answers from the SAME `terminalRefs` map the product reads, which
     * under the default arm is EMPTY. So the canonical run has one waiting tab,
     * zero mounted handles, and therefore zero deliveries — which is exactly
     * the situation `handlers.test.ts` used to pin as "`✓` with no PTY
     * reached". A stub that answered `delivered: tabIds.length` would have
     * re-created the defect inside the harness meant to catch it, which is why
     * the `approveDelivery` arm moves the MAP rather than this count.
     */
    approveAll: async (...args: unknown[]) => {
      const tabIds = (args[0] as string[]) ?? [];
      const refs = terminalRefs as Map<string, { current?: unknown }>;
      const deliveries = tabIds.map((tabId) => ({
        tabId,
        route: refs.get(tabId)?.current ? "mounted" : "by-id",
        delivered: Boolean(refs.get(tabId)?.current),
        ...(refs.get(tabId)?.current ? {} : { code: "TERMINAL_WRITE_FAILED" }),
      }));
      const report = {
        targeted: tabIds.length,
        delivered: deliveries.filter((d) => d.delivered).length,
        deliveries,
      };
      calls.push({ name: "ctx.approveAll", args, evidence: true });
      return report;
    },
    sortZones: recArm("ctx.sortZones", () =>
      arms.sortZones === "moved" ? { moved: 2, total: 2 } : { moved: 0, total: 2 },
    ),
    exportAll: recArmAsync("ctx.exportAll", () =>
      arms.exportAll === "cancelled"
        ? { exported: 0, cancelled: true }
        : { exported: 2, cancelled: false },
    ),
    openDocFinder: recArm("ctx.openDocFinder", () => ({ changed: arms.modalOpen === "opened" })),
    openPromptModal: recArm("ctx.openPromptModal", () => ({
      changed: arms.modalOpen === "opened",
    })),
    // `showCard` returns void ON PURPOSE — see `TerminalCommandsContext`. It
    // has no arm because there is nothing to arm: `/metrics` and `/history`
    // derive their verdict from the SPEC they built, not from this call. The
    // `cards` arm moves the data those specs are built from instead.
    showCard: rec("ctx.showCard", undefined),
  } as never);

  const actions = registry.getAll();
  syncTerminalRefs();
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
    arms,
    setArms(next: Partial<EffectArms>): void {
      Object.assign(arms, next);
      syncTerminalRefs();
      syncFixtures();
    },
    resetArms(): void {
      Object.assign(arms, DEFAULT_ARMS);
      syncTerminalRefs();
      syncFixtures();
    },
  };
}
