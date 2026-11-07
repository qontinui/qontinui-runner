// @vitest-environment jsdom
/**
 * The boot / session restore meeting the resource guard, entered through the
 * REAL `useTerminalInitialization` mounted in React (plan
 * `2026-11-04-unplanned-restart-restore-is-page-gated-and-counts-unverified-resumes`
 * Phase 5).
 *
 * `createTerminal` stands in for `useTerminalManager`'s, refusing one record the
 * way that function does on a guard refusal. Asked to defer (`onRefusal:
 * "defer"`), it rejects with the guard's deferral. Asked nothing, it does what
 * the attended path does on a box nobody is watching: it opens the operator
 * dialog and waits for an answer that never comes. The restore must not wait on
 * that dialog. It must finish the run, leave the page usable, and restore the
 * refused record and the ones after it on its own once the box has room, with
 * no operator in the loop.
 */

import { act, createElement, useCallback, useMemo, useRef, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

let openRecords: unknown[] = [];
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string) => {
    switch (cmd) {
      case "terminal_holder_sweep_status":
        return { success: true, data: { settled: true, held: [] } };
      case "terminal_holder_sweep_settled":
        return { success: true, data: { settled: true } };
      case "terminal_session_list_open":
        return { success: true, data: { sessions: openRecords, deferredByDrain: null } };
      case "claude_default_config_home":
        return "/home/u/.claude";
      default:
        return { success: true, data: null };
    }
  }),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
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
  loadKnownPageIds: () => ["default"],
  readPinnedPageId: () => null,
}));
vi.mock("@/lib/pageBindings", () => ({ getPageBindings: () => ({}) }));

import { ResourceGuardDeferral, parseResourceGuardRefusal } from "@/lib/resourceGuard";
import wire from "@/lib/resourceGuardWire.fixture.json";
import { useTerminalInitialization } from "./useTerminalInitialization";
import type { TerminalTab } from "./useTerminalManager";

const REFUSAL = wire.refusals.thread_count;

/** An unconfirmed (terminal-only) record, identified by its working dir. */
function shellRecord(n: number) {
  return {
    claudeSessionId: `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`,
    pageId: "default",
    zoneIndex: n,
    terminalId: `old-${n}`,
    workingDir: `/w/${n}`,
    openedAt: 1,
    lastSeenAt: 1,
    state: "open",
    origin: "authoritative",
  };
}

let refusedDirs = new Set<string>();
const spawnedDirs: string[] = [];
let dialogsOpened = 0;
let initialized = false;

function Harness() {
  const [tabs, setTabs] = useState<TerminalTab[]>([]);
  const next = useRef(0);
  const createTerminal = useCallback(
    async (
      title?: string,
      workingDir?: string,
      _tenantId?: string,
      source?: { onRefusal?: string },
    ): Promise<string | null> => {
      const dir = workingDir ?? "";
      if (refusedDirs.has(dir)) {
        if (source?.onRefusal === "defer") {
          throw new ResourceGuardDeferral(parseResourceGuardRefusal(REFUSAL)!, REFUSAL);
        }
        // The attended path: the operator dialog, which nobody answers.
        dialogsOpened += 1;
        return new Promise<string | null>(() => {});
      }
      spawnedDirs.push(dir);
      const id = `tab-${next.current++}`;
      setTabs((prev) => [
        ...prev,
        { id, title: title ?? id, pid: 1, isAlive: true, exitCode: null, workingDir },
      ]);
      return id;
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
      setRestoreSettled: () => {},
    }),
    [],
  );
  const labelsAndTags = useMemo(
    () => ({ setZoneLabel: () => {}, setZoneNote: () => {}, setPinnedZones: () => {} }),
    [],
  );
  const sessionPersistence = useMemo(
    () => ({
      saveSessionLayout: () => {},
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
  const reconnect = useCallback(async () => [] as string[], []);
  const createPlanTab = useCallback(() => null, []);
  const adoptWorkerTab = useCallback(() => null, []);
  const setInitialized = useCallback((v: boolean) => {
    if (v) initialized = true;
  }, []);
  useTerminalInitialization({
    pageId: "default",
    tabs,
    terminalRefs,
    reconnectToExistingSessions: reconnect,
    createTerminal,
    createPlanTab,
    adoptWorkerTab,
    setInitialized,
    updateTab,
    zoneLayout,
    labelsAndTags,
    sessionPersistence,
    layoutState,
  });
  return null;
}

let container: HTMLDivElement;
let root: Root;

/** Let every chained await of the restore settle, advancing fake time by `ms`. */
async function settle(ms = 0) {
  for (let i = 0; i < 30; i++) {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(i === 0 ? ms : 0);
    });
  }
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.useFakeTimers({
    toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval", "Date"],
  });
  vi.spyOn(console, "warn").mockImplementation(() => {});
  vi.spyOn(console, "error").mockImplementation(() => {});
  spawnedDirs.length = 0;
  refusedDirs = new Set();
  dialogsOpened = 0;
  initialized = false;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("useTerminalInitialization — a restore the resource guard refuses", () => {
  it("never waits on the operator dialog, and restores the rest once the box has room", async () => {
    openRecords = [shellRecord(1), shellRecord(2), shellRecord(3)];
    refusedDirs = new Set(["/w/2"]);
    await act(async () => {
      root.render(createElement(Harness));
    });
    await settle();

    // No operator dialog was opened for the restore, and the run finished:
    // the page left its loading state instead of hanging on record 2.
    expect(dialogsOpened).toBe(0);
    expect(initialized).toBe(true);
    expect(spawnedDirs).toEqual(["/w/1"]);

    // The box gains room; the restore comes back on its own and restores the
    // refused record and the one after it, with nobody answering anything.
    refusedDirs = new Set();
    await settle(15_000);
    expect(spawnedDirs).toEqual(["/w/1", "/w/2", "/w/3"]);
    expect(dialogsOpened).toBe(0);
  });
});
