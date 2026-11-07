// @vitest-environment jsdom
/**
 * A session restore the resource guard refuses, driven through the REAL
 * `useTerminalInitialization` mounted in React (plan
 * `2026-11-04-unplanned-restart-restore-is-page-gated-and-counts-unverified-resumes`
 * Phase 5). The pure pieces (`RestoreGuardRetry`, `RerunTimers`,
 * `spawnWithResourceGuard`'s defer arm) are tested beside their modules; what
 * only the mounted hook can show is the wiring between them: the loop's catch →
 * break → `finally` → `guardRetry.defer`, the retry run that follows, and what
 * a deferred run must NOT do on its way out (strand records after the refusal,
 * drop the orphan-adoption claim, delete scrollback, open the auto-save gate).
 *
 * `createTerminal` is the real composition `useTerminalManager` uses — the
 * guard's attended wrapper around a spawn, with `createTerminalFailure` as its
 * catch — over a scripted spawn, so a refusal reaches the hook exactly as a
 * runner refusal does.
 */

import { act, createElement, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invokeCalls: Array<{ cmd: string; args: unknown }> = [];
let openRecords: unknown[] = [];
/** The coord drain's deferral of the restore set, or `null` (none). */
let listDeferred: { reason: string } | null = null;
/** The persisted pages (`loadKnownPageIds`). */
let knownPages: string[] = ["default"];
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: unknown) => {
    invokeCalls.push({ cmd, args });
    switch (cmd) {
      case "terminal_holder_sweep_status":
        return { success: true, data: { settled: true, held: [] } };
      case "terminal_holder_sweep_settled":
        return { success: true, data: { settled: true } };
      case "terminal_session_list_open":
        return { success: true, data: { sessions: openRecords, deferredByDrain: listDeferred } };
      case "claude_default_config_home":
        return "/home/u/.claude";
      default:
        return { success: true, data: null };
    }
  }),
}));
const eventHandlers = new Map<string, (event: { payload: unknown }) => void>();
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, handler: (event: { payload: unknown }) => void) => {
    eventHandlers.set(name, handler);
    return () => eventHandlers.delete(name);
  }),
}));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ onCloseRequested: async () => () => {} }),
}));
vi.mock("@/hooks/useCoordDrainState", () => ({
  subscribeCoordDrainState: () => () => {},
  autonomousResumeDetector: () => () => {},
}));
vi.mock("./liveClaudeSessions", () => ({
  fetchLiveClaudeSessionIds: async () => new Set<string>(),
}));
vi.mock("./useTerminalPages", () => ({
  loadKnownPageIds: () => knownPages,
  readPinnedPageId: () => null,
}));
vi.mock("@/lib/pageBindings", () => ({ getPageBindings: () => ({}) }));

import { spawnWithResourceGuard, type ResourceGuardSource } from "@/lib/resourceGuard";
import {
  GUARD_WAIT_INITIAL_MS,
  getWaitsSnapshot,
  withdrawResourceGuardWait,
  ResourceGuardDeferral,
  parseResourceGuardRefusal,
} from "@/lib/resourceGuard";
import wire from "@/lib/resourceGuardWire.fixture.json";
import {
  GUARD_GATE_HOLD_MS,
  HOLDER_SESSION_RELEASED_EVENT,
  restoreGuardWaitKey,
  useTerminalInitialization,
} from "./useTerminalInitialization";
import { createTerminalFailure, type TerminalTab } from "./useTerminalManager";

const REFUSAL = wire.refusals.thread_count;
const uuid = (n: number) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;

/** An unconfirmed (terminal-only) record on `page`, identified by its working dir. */
function shellRecord(n: number, page = "default") {
  return {
    claudeSessionId: uuid(n),
    pageId: page,
    zoneIndex: n,
    terminalId: `old-${n}`,
    workingDir: `/w/${n}`,
    openedAt: 1,
    lastSeenAt: 1,
    state: "open",
    origin: "authoritative",
  };
}

/** A confirmed Claude record whose account is unknown: restored as `needs-account`. */
function needsAccountRecord(n: number, page: string) {
  return { ...shellRecord(n, page), confirmedAt: 1, provider: "claude" };
}

/** Working dirs the guard refuses until told otherwise. */
let refusedDirs = new Set<string>();
/** What the reconnect answers: `null` is indeterminate, which aborts the run. */
let reconnectResult: string[] | null = [];
const spawnedDirs: string[] = [];
const saves: unknown[] = [];
/** The hook's published per-page restore completion (the auto-save gate), latest commit. */
const observed: { completePages: ReadonlySet<string>; trimSettled: boolean[] } = {
  completePages: new Set(),
  trimSettled: [],
};

function Harness({
  pageId,
  onInitialized,
}: {
  pageId: string;
  onInitialized: (v: boolean) => void;
}) {
  const [tabs, setTabs] = useState<TerminalTab[]>([]);
  const next = useRef(0);
  const createTerminal = useCallback(
    async (
      title?: string,
      workingDir?: string,
      _tenantId?: string,
      source?: ResourceGuardSource,
    ): Promise<string | null> => {
      try {
        return await spawnWithResourceGuard(async () => {
          if (refusedDirs.has(workingDir ?? "")) throw REFUSAL;
          spawnedDirs.push(workingDir ?? "");
          const id = `tab-${next.current++}`;
          setTabs((prev) => [
            ...prev,
            { id, title: title ?? id, pid: 1, isAlive: true, exitCode: null, workingDir },
          ]);
          return id;
        }, source);
      } catch (err) {
        return createTerminalFailure(err);
      }
    },
    [],
  );
  const updateTab = useCallback((id: string, updates: Partial<TerminalTab>) => {
    setTabs((prev) => prev.map((t) => (t.id === id ? { ...t, ...updates } : t)));
  }, []);
  const terminalRefs = useRef(new Map());
  const zoneLayout = useMemo(
    () => ({
      layoutId: "grid-2x2",
      setLayoutId: () => {},
      placeRestoredTab: () => {},
      setFocusedZone: () => {},
      readGridView: () => ({ layoutId: "grid-2x2", assignments: {} }),
      setRestoreSettled: (settled: boolean) => observed.trimSettled.push(settled),
    }),
    [],
  );
  const labelsAndTags = useMemo(
    () => ({ setZoneLabel: () => {}, setZoneNote: () => {}, setPinnedZones: () => {} }),
    [],
  );
  const sessionPersistence = useMemo(
    () => ({
      saveSessionLayout: (p: unknown) => saves.push(p),
      saveScrollbackBuffers: async () => ({}),
      updateScrollbackPaths: () => {},
      getSavedLayout: () => null,
      clearSavedLayout: () => {},
      hasSavedLayout: () => false,
    }),
    [],
  );
  const layoutState = useMemo(
    () => ({
      layoutId: "grid-2x2",
      assignments: {},
      zoneLabels: {},
      zoneNotes: {},
      pinnedZones: new Set<number>(),
      focusedZone: 0,
    }),
    [],
  );
  const reconnect = useCallback(async () => reconnectResult, []);
  // Stable, as `useTerminalManager`'s are: the init effect depends on them.
  const createPlanTab = useCallback(() => null, []);
  const adoptWorkerTab = useCallback(() => null, []);
  const restore = useTerminalInitialization({
    pageId,
    tabs,
    terminalRefs,
    reconnectToExistingSessions: reconnect,
    createTerminal,
    createPlanTab,
    adoptWorkerTab,
    setInitialized: onInitialized,
    updateTab,
    zoneLayout,
    labelsAndTags,
    sessionPersistence,
    layoutState,
  });
  const complete = restore.restoreCompletePages;
  useEffect(() => {
    observed.completePages = complete;
  }, [complete]);
  return null;
}

let container: HTMLDivElement;
let root: Root;
let initialized: boolean[];

/** Let every chained await of the restore settle, advancing fake time by `ms`. */
async function settle(ms = 0) {
  for (let i = 0; i < 30; i++) {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(i === 0 ? ms : 0);
    });
  }
}

async function mount(pageId = "default") {
  await act(async () => {
    root.render(createElement(Harness, { pageId, onInitialized: (v) => initialized.push(v) }));
  });
  await settle();
}

/** Advance fake time in steps, letting every restore run it triggers settle. */
async function elapse(ms: number, step = 5_000) {
  for (let t = 0; t < ms; t += step) await settle(Math.min(step, ms - t));
}

const heldForAccount = (n: number) =>
  invokeCalls.filter(
    (c) =>
      c.cmd === "terminal_session_mark_awaiting_account" &&
      (c.args as { claudeSessionId?: string }).claudeSessionId === uuid(n),
  );

const cleanupCalls = () => invokeCalls.filter((c) => c.cmd === "terminal_cleanup_scrollback");

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.useFakeTimers({
    toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval", "Date"],
  });
  vi.spyOn(console, "warn").mockImplementation(() => {});
  vi.spyOn(console, "error").mockImplementation(() => {});
  invokeCalls.length = 0;
  spawnedDirs.length = 0;
  saves.length = 0;
  observed.completePages = new Set();
  observed.trimSettled = [];
  refusedDirs = new Set();
  reconnectResult = [];
  listDeferred = null;
  knownPages = ["default"];
  initialized = [];
  eventHandlers.clear();
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  for (const w of getWaitsSnapshot()) withdrawResourceGuardWait(w.key);
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("createTerminalFailure — what createTerminal answers for a spawn that threw", () => {
  it("re-throws a guard deferral, so the deferring restore can retry it", () => {
    const deferral = new ResourceGuardDeferral(parseResourceGuardRefusal(REFUSAL)!, REFUSAL);
    expect(() => createTerminalFailure(deferral)).toThrow(deferral);
  });

  it("answers null for any other failure", () => {
    expect(createTerminalFailure("Failed to open PTY: os error 5")).toBeNull();
  });
});

describe("useTerminalInitialization — a guard-refused restore (mounted hook)", () => {
  it("stops at the refused record, settles the run, then retries the rest merging", async () => {
    openRecords = [shellRecord(1), shellRecord(2), shellRecord(3)];
    refusedDirs = new Set(["/w/2"]);
    await mount();

    // The loop stopped at the refusal: record 3 was not attempted.
    expect(spawnedDirs).toEqual(["/w/1"]);
    // The run's `finally` ran: the page left its loading state…
    expect(initialized).toContain(true);
    // …and the retry is published as a wait, not held inside the run.
    expect(getWaitsSnapshot()).toMatchObject([
      {
        key: restoreGuardWaitKey("default"),
        sourceLabel: "session restore",
        metric: "thread_count",
        refusals: 1,
      },
    ]);

    // Room appears; the retry is a merging run that skips record 1's live tab.
    refusedDirs = new Set();
    await settle(GUARD_WAIT_INITIAL_MS);
    expect(spawnedDirs).toEqual(["/w/1", "/w/2", "/w/3"]);
    expect(getWaitsSnapshot()).toEqual([]);
  });

  it("a recheck-only run while a retry is owed restores in full, never stranding the rest (B1)", async () => {
    // The page restores once, fully.
    openRecords = [shellRecord(1)];
    await mount();
    expect(spawnedDirs).toEqual(["/w/1"]);

    // A released holder hands back session 2; its recheck run is refused.
    openRecords = [shellRecord(1), shellRecord(2), shellRecord(3)];
    refusedDirs = new Set(["/w/2"]);
    const released = eventHandlers.get(HOLDER_SESSION_RELEASED_EVENT)!;
    await act(async () =>
      released({ payload: { sessionId: uuid(2), terminalId: "old-2", pageId: "default" } }),
    );
    await settle();
    expect(spawnedDirs).toEqual(["/w/1"]);
    expect(getWaitsSnapshot()).toHaveLength(1);

    // Before the retry fires, another release (session 3) re-runs the page.
    // That run must be the owed retry — in full — not a recheck of 3 alone.
    refusedDirs = new Set();
    await act(async () =>
      released({ payload: { sessionId: uuid(3), terminalId: "old-3", pageId: "default" } }),
    );
    await settle();
    expect(spawnedDirs.sort()).toEqual(["/w/1", "/w/2", "/w/3"]);
    expect(getWaitsSnapshot()).toEqual([]);
  });

  it("gives the orphan-adoption claim back, so the retry still adopts unreached orphans (B2)", async () => {
    // Record 1 is refused; the orphan (on a page that no longer exists) comes
    // after it and is never reached by the first run.
    openRecords = [shellRecord(1), needsAccountRecord(9, "gone-page")];
    refusedDirs = new Set(["/w/1"]);
    await mount();
    expect(heldForAccount(9)).toEqual([]);

    refusedDirs = new Set();
    await settle(GUARD_WAIT_INITIAL_MS);
    expect(spawnedDirs).toEqual(["/w/1"]);
    // The retry adopted the orphan and held it for an account choice.
    expect(heldForAccount(9)).toHaveLength(1);
  });

  it("the orphan-adoption claim is the deferred page's: another page never adopts its orphans (W-1)", async () => {
    // The orphan comes first, so the default page adopts it before the refusal.
    openRecords = [needsAccountRecord(9, "gone-page"), shellRecord(1)];
    refusedDirs = new Set(["/w/1"]);
    await mount("default");
    expect(heldForAccount(9)).toHaveLength(1);
    expect(getWaitsSnapshot()).toHaveLength(1);

    // Another page's first restore while the default page is owed its retry:
    // it must not adopt the orphan the default page already restored.
    await mount("page-2");
    expect(heldForAccount(9)).toHaveLength(1);
  });

  it("a later drain-deferred run of the owner keeps the claim: another page never re-adopts its orphans", async () => {
    knownPages = ["page-a", "page-b"];
    openRecords = [needsAccountRecord(8, "gone-page"), needsAccountRecord(9, "gone-page")];
    await mount("page-a");
    expect(heldForAccount(8)).toHaveLength(1);
    expect(heldForAccount(9)).toHaveLength(1);

    // A re-run of page A (a released holder) is deferred by the coord drain.
    listDeferred = { reason: "device drained" };
    const released = eventHandlers.get(HOLDER_SESSION_RELEASED_EVENT)!;
    await act(async () =>
      released({ payload: { sessionId: uuid(8), terminalId: "old-8", pageId: "page-a" } }),
    );
    await settle();
    listDeferred = null;

    // Page B's first restore must not adopt the orphans A already restored.
    await mount("page-b");
    expect(heldForAccount(8)).toHaveLength(1);
    expect(heldForAccount(9)).toHaveLength(1);
  });

  it("opens the auto-save gate after the hold even while the retry is still refused (W-2)", async () => {
    openRecords = [shellRecord(1), shellRecord(2)];
    refusedDirs = new Set(["/w/2"]);
    await mount();
    await elapse(GUARD_GATE_HOLD_MS - 10_000);
    expect(observed.completePages.has("default")).toBe(false);
    await elapse(20_000);
    expect(observed.completePages.has("default")).toBe(true);
    // The box is still full: the retry is still owed, and the scrollback kept.
    expect(getWaitsSnapshot()).toHaveLength(1);
    expect(cleanupCalls()).toEqual([]);
  });

  it("the hold's expiry releases the trim hold with the auto-save gate", async () => {
    openRecords = [shellRecord(1), shellRecord(2)];
    refusedDirs = new Set(["/w/2"]);
    await mount();
    await elapse(GUARD_GATE_HOLD_MS - 10_000);
    expect(observed.trimSettled.at(-1)).toBe(false);
    await elapse(20_000);
    expect(observed.completePages.has("default")).toBe(true);
    expect(observed.trimSettled.at(-1)).toBe(true);
  });

  it("the unmount save does not write the partial layout while the retry is owed (W-2)", async () => {
    openRecords = [shellRecord(1), shellRecord(2)];
    refusedDirs = new Set(["/w/2"]);
    await mount();
    expect(spawnedDirs).toEqual(["/w/1"]);
    act(() => root.unmount());
    expect(saves).toEqual([]);
    // afterEach unmounts again; give it a fresh root.
    root = createRoot(container);
  });

  it("an aborted retry run re-arms the retry and its wait (S-1)", async () => {
    openRecords = [shellRecord(1), shellRecord(2)];
    refusedDirs = new Set(["/w/2"]);
    await mount();

    // The retry run's reconnect is indeterminate: it aborts, restoring nothing.
    refusedDirs = new Set();
    reconnectResult = null;
    await settle(GUARD_WAIT_INITIAL_MS);
    expect(spawnedDirs).toEqual(["/w/1"]);
    expect(getWaitsSnapshot()).toMatchObject([{ retryAt: expect.any(Number) }]);

    // The re-armed retry restores the rest.
    reconnectResult = [];
    await elapse(GUARD_WAIT_INITIAL_MS, 1_000);
    expect(spawnedDirs).toEqual(["/w/1", "/w/2"]);
    expect(getWaitsSnapshot()).toEqual([]);
  });

  it("keeps the scrollback and the auto-save gate until the retry restores the rest (W1)", async () => {
    openRecords = [shellRecord(1), shellRecord(2)];
    refusedDirs = new Set(["/w/2"]);
    await mount();
    // Past the drain timer for the tab that WAS created.
    await settle(2_000);
    expect(cleanupCalls()).toEqual([]);
    expect(observed.completePages.has("default")).toBe(false);
    expect(saves).toEqual([]);

    refusedDirs = new Set();
    await settle(GUARD_WAIT_INITIAL_MS);
    await settle(2_000);
    expect(spawnedDirs).toEqual(["/w/1", "/w/2"]);
    // The retry restored the rest: now the cleanup runs and the gate opens.
    expect(cleanupCalls()).toHaveLength(1);
    expect(observed.completePages.has("default")).toBe(true);
  });
});
