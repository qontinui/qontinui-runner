// @vitest-environment jsdom
/**
 * Removing a terminal page while the session restore still holds state for it,
 * entered through the REAL `useTerminalPages().removePage` beside the REAL
 * `useTerminalInitialization` — the way `TerminalPage` composes them (its page
 * is the page list's active page). Plan
 * `2026-11-04-unplanned-restart-restore-is-page-gated-and-counts-unverified-resumes`
 * Phase 5.
 *
 * What a removal must do: release the orphan-adoption claim the page held and
 * adopt the orphans it never reached onto the page on screen; end its guard
 * retry (timer, wait, hold) for good, even when a run of it is in flight and
 * settles afterwards; and NEVER bring back a session of the page the operator
 * just closed — its own records, or an orphan it had restored.
 *
 * `createTerminal` stands in for `useTerminalManager`'s: it refuses a record
 * the way that function does on a guard refusal (rejecting with the guard's
 * deferral), and a test can hold a spawn open to remove the page mid-run.
 */

import { act, createElement, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invokeCalls: Array<{ cmd: string; args: unknown }> = [];
let openRecords: unknown[] = [];
/** What the page list's reconcile read sees (none unless a test says so). */
let reconcileRecords: unknown[] = [];
/** The live terminals `terminal_list` reports. */
let liveTerminals: Array<{ id: string; pageId: string }> = [];
/** While set, the restore's registry read fails. */
let restoreReadDown = false;
/** While set, the next restore registry read waits for it. */
let restoreReadGate: Promise<void> | null = null;
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: { purpose?: string }) => {
    invokeCalls.push({ cmd, args });
    switch (cmd) {
      case "terminal_holder_sweep_status":
        return { success: true, data: { settled: true, held: [] } };
      case "terminal_holder_sweep_settled":
        return { success: true, data: { settled: true } };
      case "terminal_list":
        return terminalListDown
          ? { success: false, error: "unreadable" }
          : { success: true, data: { terminals: liveTerminals } };
      case "terminal_session_list_open":
        if (args?.purpose === "restore") {
          const gate = restoreReadGate;
          restoreReadGate = null;
          if (gate) await gate;
          if (restoreReadDown) return { success: false, error: "registry unreadable" };
        }
        // The restore's own read sees the records. The page list's reconcile
        // read (no `purpose`) would synthesize a tab for every page a record
        // names, and an orphan is exactly a record whose page has no tab — so
        // it sees none here unless a test hands it some.
        return {
          success: true,
          data: {
            sessions: args?.purpose === "restore" ? openRecords : reconcileRecords,
            deferredByDrain: null,
          },
        };
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
  COORD_DRAIN_STATE_EVENT: "coord-drain-state-changed",
  subscribeCoordDrainState: () => () => {},
  autonomousResumeDetector: () => () => {},
}));
/** While set, the next read of the live Claude sessions waits for it. */
let liveSessionsGate: Promise<void> | null = null;
vi.mock("./liveClaudeSessions", () => ({
  fetchLiveClaudeSessionIds: async () => {
    const gate = liveSessionsGate;
    liveSessionsGate = null;
    if (gate) await gate;
    return new Set<string>();
  },
}));

import {
  GUARD_WAIT_INITIAL_MS,
  ResourceGuardDeferral,
  getWaitsSnapshot,
  parseResourceGuardRefusal,
  withdrawResourceGuardWait,
} from "@/lib/resourceGuard";
import { instanceStorage } from "@/lib/instance-storage";
import wire from "@/lib/resourceGuardWire.fixture.json";
import {
  ORPHAN_PASS_MAX_ATTEMPTS,
  restoreGuardWaitKey,
  useTerminalInitialization,
  type UnadoptedOrphans,
} from "./useTerminalInitialization";
import type { TerminalTab } from "./useTerminalManager";
import { PAGES_STORAGE_KEY, useTerminalPages } from "./useTerminalPages";

const REFUSAL = wire.refusals.thread_count;
const uuid = (n: number) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const deferral = () => new ResourceGuardDeferral(parseResourceGuardRefusal(REFUSAL)!, REFUSAL);

/** An unconfirmed (terminal-only) record, identified by its working dir. */
function shellRecord(n: number, page: string) {
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

/** A confirmed Claude record under the default account: restored `auto-resume`. */
function resumableRecord(n: number, page: string) {
  return {
    ...shellRecord(n, page),
    confirmedAt: 1,
    provider: "claude",
    configDir: "/home/u/.claude",
  };
}

/** A confirmed Claude record whose account is unknown: restored as `needs-account`. */
function needsAccountRecord(n: number, page: string) {
  return { ...shellRecord(n, page), confirmedAt: 1, provider: "claude" };
}

const heldForAccount = (n: number) =>
  invokeCalls.filter(
    (c) =>
      c.cmd === "terminal_session_mark_awaiting_account" &&
      (c.args as { claudeSessionId?: string }).claudeSessionId === uuid(n),
  );

/** Working dirs refused with the guard's deferral. */
let refusedDirs = new Set<string>();
/** Working dirs whose spawn is held open until the test settles it. */
const heldSpawns = new Map<
  string,
  { resolve: (id: string | null) => void; reject: (e: unknown) => void }
>();
let holdDirs = new Set<string>();
const spawnedDirs: string[] = [];
/** Every spawn the restore asked for, refused or not. */
const attemptedDirs: string[] = [];
/** The tab ids each working dir was spawned as, in order. */
const tabsOfDir = new Map<string, string[]>();
/** While set, `terminal_list` cannot be read. */
let terminalListDown = false;

const captured: {
  setActivePageId: ((id: string) => void) | null;
  removePage: ((id: string) => Promise<void>) | null;
  unadopted: readonly UnadoptedOrphans[];
  complete: ReadonlySet<string>;
} = { setActivePageId: null, removePage: null, unadopted: [], complete: new Set() };
/** Each page's trim hold, as its zone layout last heard: `true` = released. */
const trimSettled = new Map<string, boolean>();

function Harness() {
  const pages = useTerminalPages();
  const [tabs, setTabs] = useState<TerminalTab[]>([]);
  const next = useRef(0);
  const addTab = useCallback((title: string | undefined, workingDir: string | undefined) => {
    spawnedDirs.push(workingDir ?? "");
    const id = `tab-${next.current++}`;
    tabsOfDir.set(workingDir ?? "", [...(tabsOfDir.get(workingDir ?? "") ?? []), id]);
    setTabs((prev) => [
      ...prev,
      { id, title: title ?? id, pid: 1, isAlive: true, exitCode: null, workingDir },
    ]);
    return id;
  }, []);
  const createTerminal = useCallback(
    async (title?: string, workingDir?: string): Promise<string | null> => {
      const dir = workingDir ?? "";
      attemptedDirs.push(dir);
      if (refusedDirs.has(dir)) throw deferral();
      if (holdDirs.has(dir)) {
        holdDirs.delete(dir);
        const settled = await new Promise<string | null>((resolve, reject) =>
          heldSpawns.set(dir, { resolve, reject }),
        );
        return settled === null ? null : addTab(title, workingDir);
      }
      return addTab(title, workingDir);
    },
    [addTab],
  );
  const updateTab = useCallback((id: string, updates: Partial<TerminalTab>) => {
    setTabs((prev) => prev.map((t) => (t.id === id ? { ...t, ...updates } : t)));
  }, []);
  const terminalRefs = useRef(new Map());
  // Each page's own zone layout, as each page scope has one.
  const layoutPage = pages.activePageId;
  const zoneLayout = useMemo(
    () => ({
      layoutId: "grid-2x2",
      setLayoutId: () => {},
      placeRestoredTab: () => {},
      setFocusedZone: () => {},
      readGridView: () => ({ layoutId: "grid-2x2", assignments: {} }),
      setRestoreSettled: (settled: boolean) => trimSettled.set(layoutPage, settled),
    }),
    [layoutPage],
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
  const setInitialized = useCallback(() => {}, []);
  const { unadoptedOrphans, restoreCompletePages } = useTerminalInitialization({
    pageId: pages.activePageId,
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
  const { setActivePageId, removePage } = pages;
  useEffect(() => {
    captured.unadopted = unadoptedOrphans;
    captured.complete = restoreCompletePages;
  }, [unadoptedOrphans, restoreCompletePages]);
  useEffect(() => {
    captured.setActivePageId = setActivePageId;
    captured.removePage = removePage;
  }, [setActivePageId, removePage]);
  return null;
}

let container: HTMLDivElement;
let root: Root;

/** Let every chained await settle, advancing fake time by `ms`. */
async function settle(ms = 0) {
  for (let i = 0; i < 30; i++) {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(i === 0 ? ms : 0);
    });
  }
}

async function mountOn(activePage: string, pageIds: string[] = ["page-a", "page-b"]) {
  instanceStorage.setJSON(
    PAGES_STORAGE_KEY,
    pageIds.map((id, i) => ({ id, name: id, createdAt: i + 1 })),
  );
  instanceStorage.setItem("qontinui-terminal-active-page", activePage);
  await act(async () => {
    root.render(createElement(Harness));
  });
  await settle();
}

async function switchTo(page: string) {
  await act(async () => captured.setActivePageId!(page));
  await settle();
}

async function removePage(page: string) {
  await act(async () => {
    await captured.removePage!(page);
  });
  await settle();
}

/** Re-create `page` through the page list's real path, which activates it. */
async function recreate(page: string) {
  await act(async () => {
    window.dispatchEvent(
      new CustomEvent("terminal-open-page", { detail: { pageId: page, name: page } }),
    );
  });
  await settle();
}

const closedIds = () =>
  invokeCalls
    .filter((c) => c.cmd === "terminal_close")
    .map((c) => (c.args as { terminalId?: string }).terminalId);

const waitFor = (page: string) =>
  getWaitsSnapshot().find((w) => w.key === restoreGuardWaitKey(page));

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.useFakeTimers({
    toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval", "Date"],
  });
  vi.spyOn(console, "warn").mockImplementation(() => {});
  vi.spyOn(console, "error").mockImplementation(() => {});
  localStorage.clear();
  invokeCalls.length = 0;
  spawnedDirs.length = 0;
  attemptedDirs.length = 0;
  reconcileRecords = [];
  liveTerminals = [];
  tabsOfDir.clear();
  terminalListDown = false;
  liveSessionsGate = null;
  restoreReadDown = false;
  restoreReadGate = null;
  captured.unadopted = [];
  captured.complete = new Set();
  trimSettled.clear();
  heldSpawns.clear();
  holdDirs = new Set();
  refusedDirs = new Set();
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  window.history.replaceState(null, "", "/");
  act(() => root.unmount());
  container.remove();
  for (const w of getWaitsSnapshot()) withdrawResourceGuardWait(w.key);
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("removing a page through useTerminalPages.removePage", () => {
  it("after the others restored: the owner's unreached orphans are adopted, its retry ends, and its own sessions stay closed", async () => {
    // Page A owns the claim: it adopts orphan 8, is refused at its own record
    // 1, and never reaches orphan 9 or its own needs-account record 10.
    openRecords = [
      needsAccountRecord(8, "gone-page"),
      shellRecord(1, "page-a"),
      needsAccountRecord(9, "gone-page"),
      needsAccountRecord(10, "page-a"),
    ];
    refusedDirs = new Set(["/w/1"]);
    await mountOn("page-a");
    expect(heldForAccount(8)).toHaveLength(1);
    expect(heldForAccount(9)).toEqual([]);
    expect(waitFor("page-a")).toBeDefined();

    // Page B restores; it adopts nothing — A owns the claim.
    await switchTo("page-b");
    expect(heldForAccount(9)).toEqual([]);

    // A is removed. No page's FIRST restore is left to run.
    await removePage("page-a");

    // The orphan A never reached is adopted at once, by the page on screen —
    // and A's own record, unreached and still open, is NOT: closing a page
    // never brings its sessions back.
    expect(heldForAccount(9)).toHaveLength(1);
    expect(heldForAccount(10)).toEqual([]);
    // …nor is orphan 8, which A had already restored: it closed with A.
    expect(heldForAccount(8)).toHaveLength(1);
    // A's wait is gone and stays gone: its retry timer never fires (firing
    // republishes the wait), and its refused record is never spawned.
    expect(waitFor("page-a")).toBeUndefined();
    refusedDirs = new Set();
    await settle(10 * GUARD_WAIT_INITIAL_MS);
    expect(waitFor("page-a")).toBeUndefined();
    expect(spawnedDirs).not.toContain("/w/1");
  });

  it("a run of the removed page refused after the removal brings no retry or wait back", async () => {
    openRecords = [shellRecord(1, "page-a")];
    holdDirs = new Set(["/w/1"]);
    await mountOn("page-a");
    expect(heldSpawns.has("/w/1")).toBe(true);
    await switchTo("page-b");

    // Removed while its run waits on the spawn — which the guard then refuses.
    await removePage("page-a");
    await act(async () => heldSpawns.get("/w/1")!.reject(deferral()));
    await settle();

    expect(waitFor("page-a")).toBeUndefined();
    await settle(10 * GUARD_WAIT_INITIAL_MS);
    expect(waitFor("page-a")).toBeUndefined();
    expect(spawnedDirs).toEqual([]);
  });

  it("a run of the removed page in flight at the removal restores nothing more", async () => {
    openRecords = [shellRecord(1, "page-a"), shellRecord(2, "page-a")];
    holdDirs = new Set(["/w/1"]);
    await mountOn("page-a");
    expect(heldSpawns.has("/w/1")).toBe(true);
    await switchTo("page-b");

    // Removed while its run waits on record 1's spawn, which then succeeds:
    // the run must not go on to restore record 2 of a page that is gone.
    await removePage("page-a");
    await act(async () => heldSpawns.get("/w/1")!.resolve("ok"));
    await settle();

    expect(spawnedDirs).not.toContain("/w/2");
    // Record 1's pane landed after the removal, and record 1 is the removed
    // page's own: it is closed, and nothing brings the session back.
    expect(closedIds()).toContain(tabsOfDir.get("/w/1")![0]);
    await settle(5_000);
    expect(spawnedDirs).toEqual(["/w/1"]);
  });

  it("a run of the removed page that ends after the removal runs no resume drain, so it deletes no scrollback", async () => {
    // A restores record 1, and is mid-spawn on record 2 when it is removed.
    openRecords = [shellRecord(1, "page-a"), shellRecord(2, "page-a")];
    holdDirs = new Set(["/w/2"]);
    await mountOn("page-a");
    expect(spawnedDirs).toEqual(["/w/1"]);
    await switchTo("page-b");
    await removePage("page-a");
    const before = invokeCalls.length;

    // Its drain's cleanup deletes EVERY saved scrollback, the ones of the
    // sessions it never reached among them — sessions another page restores
    // now (a page re-created under its id, or the adoption pass).
    await act(async () => heldSpawns.get("/w/2")!.resolve("ok"));
    await settle();
    await settle(5_000);
    expect(invokeCalls.slice(before).map((c) => c.cmd)).not.toContain(
      "terminal_cleanup_scrollback",
    );
  });

  it("a removed page id that exists again restores, and its guard retry arms, as any page does", async () => {
    // "default" is removed, then re-created through the real path the page
    // list uses for an explicit page (`openPage`, via `terminal-open-page`),
    // as the reconcile does when a terminal lands on it.
    // "default" earns a tab only while it hosts a session, so the reconcile
    // sees the record that makes it occupied.
    openRecords = [shellRecord(1, "default")];
    reconcileRecords = openRecords;
    refusedDirs = new Set(["/w/1"]);
    await mountOn("page-a", ["page-a", "default"]);
    expect(attemptedDirs).toEqual([]);
    await removePage("default");
    await act(async () => {
      window.dispatchEvent(
        new CustomEvent("terminal-open-page", { detail: { pageId: "default", name: "Terminal" } }),
      );
    });
    await settle();

    // The new page's restore runs: it reaches its record, and the refusal arms
    // the retry and its wait like any page's.
    expect(attemptedDirs).toEqual(["/w/1"]);
    expect(waitFor("default")).toBeDefined();
    refusedDirs = new Set();
    await settle(GUARD_WAIT_INITIAL_MS);
    expect(spawnedDirs).toEqual(["/w/1"]);
  });

  it("an orphan's pane that lands after the removal is closed, and the orphan is adopted by the page on screen", async () => {
    // A adopts resumable orphan 9 and is mid-spawn on it when A is removed;
    // B's own restore is mid-spawn too.
    openRecords = [resumableRecord(9, "gone-page"), shellRecord(2, "page-b")];
    holdDirs = new Set(["/w/9", "/w/2"]);
    await mountOn("page-a");
    expect(heldSpawns.has("/w/9")).toBe(true);
    await switchTo("page-b");
    expect(heldSpawns.has("/w/2")).toBe(true);

    await removePage("page-a");
    // A's pane for orphan 9 lands after the removal: bound to the removed
    // page, it is closed at once.
    await act(async () => heldSpawns.get("/w/9")!.resolve("ok"));
    await settle();
    const [spare] = tabsOfDir.get("/w/9")!;
    expect(closedIds()).toContain(spare);

    // The orphan's page is gone, so the page on screen adopts it, once B's
    // run in flight has ended — onto a pane of its own, which stays open.
    await act(async () => heldSpawns.get("/w/2")!.resolve("ok"));
    await settle();
    await settle(5_000);
    const panes = tabsOfDir.get("/w/9")!;
    expect(panes).toHaveLength(2);
    expect(closedIds()).not.toContain(panes[1]);
  });

  it("a late pane of a page that exists again is closed, and that page restores the session", async () => {
    // A's run is mid-spawn on its own record 1 when A is removed; A is then
    // re-created, and its new run skips record 1, which the old run still
    // claims for its cold resume.
    openRecords = [shellRecord(1, "page-a")];
    holdDirs = new Set(["/w/1"]);
    await mountOn("page-a");
    await switchTo("page-b");
    await removePage("page-a");
    await recreate("page-a");
    expect(attemptedDirs).toEqual(["/w/1"]);

    // The old pane lands: it is closed, and the re-created A restores record 1
    // onto a pane of its own, which stays open.
    await act(async () => heldSpawns.get("/w/1")!.resolve("ok"));
    await settle();
    await settle(5_000);
    const panes = tabsOfDir.get("/w/1")!;
    expect(panes).toHaveLength(2);
    expect(closedIds()).toContain(panes[0]);
    expect(closedIds()).not.toContain(panes[1]);
  });

  it("the adoption pass waits only for the removed page's own runs, not for a run of the page re-created under its id", async () => {
    // A's run is mid-spawn on its own record 1, before orphan 9, when A is
    // removed. A is re-created and its new run is mid-spawn on record 2.
    openRecords = [
      shellRecord(1, "page-a"),
      shellRecord(2, "page-a"),
      needsAccountRecord(9, "gone-page"),
    ];
    holdDirs = new Set(["/w/1"]);
    await mountOn("page-a");
    await switchTo("page-b");
    await removePage("page-a");
    holdDirs = new Set(["/w/2"]);
    await recreate("page-a");
    expect(heldSpawns.has("/w/2")).toBe(true);
    await switchTo("page-b");

    // The removed page's run ends: the pass adopts orphan 9 onto B at once,
    // with the new A's run still in flight.
    await act(async () => heldSpawns.get("/w/1")!.resolve("ok"));
    await settle();
    expect(heldForAccount(9)).toHaveLength(1);
  });

  it("the adoption pass waits for the removed page's run to end, and takes nothing that run held", async () => {
    // A's run is holding orphan 9 for an account choice — waiting on the live
    // session read that hold makes — when A is removed.
    let open!: () => void;
    liveSessionsGate = new Promise<void>((r) => (open = r));
    openRecords = [needsAccountRecord(9, "gone-page")];
    await mountOn("page-a");
    expect(heldForAccount(9)).toEqual([]);
    await switchTo("page-b");
    await removePage("page-a");

    // The hold completes after the removal: the session is held once, and the
    // pass does not adopt it again.
    await act(async () => open());
    await settle();
    await settle(5_000);
    expect(heldForAccount(9)).toHaveLength(1);
  });

  it("an adoption pass whose target page is removed during its reads adopts onto the page then on screen", async () => {
    // B holds its own record 5 for an account choice.
    openRecords = [
      shellRecord(1, "page-a"),
      needsAccountRecord(5, "page-b"),
      needsAccountRecord(9, "gone-page"),
    ];
    refusedDirs = new Set(["/w/1"]);
    await mountOn("page-a", ["page-a", "page-b", "page-c"]);
    await switchTo("page-b");
    expect(heldForAccount(5)).toHaveLength(1);

    // The pass for A reads with B on screen; B is removed before it has read.
    let open!: () => void;
    restoreReadGate = new Promise<void>((r) => (open = r));
    await removePage("page-a");
    await removePage("page-b");
    await act(async () => open());
    await settle();
    await settle(GUARD_WAIT_INITIAL_MS);
    expect(heldForAccount(9)).toHaveLength(1);
    // B's own record closed with B: no pass brings it back onto C.
    expect(heldForAccount(5)).toHaveLength(1);
  });

  it("an adoption pass whose registry read fails stays owed, and adopts once it reads", async () => {
    openRecords = [shellRecord(1, "page-a"), needsAccountRecord(9, "gone-page")];
    refusedDirs = new Set(["/w/1"]);
    await mountOn("page-a");
    await switchTo("page-b");

    restoreReadDown = true;
    await removePage("page-a");
    expect(heldForAccount(9)).toEqual([]);

    restoreReadDown = false;
    await settle(GUARD_WAIT_INITIAL_MS);
    expect(heldForAccount(9)).toHaveLength(1);
  });

  it("an adoption pass that throws stays owed, and adopts on its retry", async () => {
    // Reading orphan 9's page throws once: at the pass's own read.
    let throwOnce = false;
    const orphan = needsAccountRecord(9, "gone-page") as Record<string, unknown>;
    delete orphan.pageId;
    Object.defineProperty(orphan, "pageId", {
      enumerable: true,
      get() {
        if (throwOnce) {
          throwOnce = false;
          throw new Error("unreadable record");
        }
        return "gone-page";
      },
    });
    openRecords = [shellRecord(1, "page-a"), orphan];
    refusedDirs = new Set(["/w/1"]);
    await mountOn("page-a");
    await switchTo("page-b");

    throwOnce = true;
    await removePage("page-a");
    expect(heldForAccount(9)).toEqual([]);
    await settle(GUARD_WAIT_INITIAL_MS);
    expect(heldForAccount(9)).toHaveLength(1);
  });

  it("an adoption pass that never reads gives up after its bounded retries, and says whose orphans it left", async () => {
    openRecords = [shellRecord(1, "page-a"), needsAccountRecord(9, "gone-page")];
    refusedDirs = new Set(["/w/1"]);
    await mountOn("page-a");
    await switchTo("page-b");
    restoreReadDown = true;
    const before = invokeCalls.length;
    const passReads = () =>
      invokeCalls
        .slice(before)
        .filter(
          (c) =>
            c.cmd === "terminal_session_list_open" &&
            (c.args as { purpose?: string } | undefined)?.purpose === "restore",
        ).length;

    await removePage("page-a");
    for (let i = 0; i < ORPHAN_PASS_MAX_ATTEMPTS; i++) await settle(120_000);
    expect(passReads()).toBe(ORPHAN_PASS_MAX_ATTEMPTS);
    expect(captured.unadopted).toEqual([{ removedPageIds: ["page-a"], sessionIds: null }]);

    // Given up for good: no further reads, however long.
    await settle(30 * 60_000);
    expect(passReads()).toBe(ORPHAN_PASS_MAX_ATTEMPTS);
  });

  it("an adoption pass after the page on screen is removed adopts onto a page that exists, in one read", async () => {
    // A window pinned to page B: removing B leaves it on the "default"
    // fallback, which the saved page list does not name, and nothing moves it.
    window.history.replaceState(null, "", "/?page=page-b");
    openRecords = [shellRecord(1, "page-b"), needsAccountRecord(9, "gone-page")];
    refusedDirs = new Set(["/w/1"]);
    await mountOn("page-b");
    const before = invokeCalls.length;
    const restoreReads = () =>
      invokeCalls
        .slice(before)
        .filter(
          (c) =>
            c.cmd === "terminal_session_list_open" &&
            (c.args as { purpose?: string } | undefined)?.purpose === "restore",
        ).length;

    await removePage("page-b");
    await settle(30 * 60_000);
    // The "default" fallback's own first restore, and the pass's one read.
    expect(restoreReads()).toBe(2);
  });

  it("an adoption pass that gave up releases the claim, so the next page to restore adopts", async () => {
    // A never reaches orphan 9, nor its own needs-account record 10.
    openRecords = [
      shellRecord(1, "page-a"),
      needsAccountRecord(9, "gone-page"),
      needsAccountRecord(10, "page-a"),
    ];
    refusedDirs = new Set(["/w/1"]);
    await mountOn("page-a", ["page-a", "page-b", "page-c"]);
    await switchTo("page-b");
    restoreReadDown = true;
    await removePage("page-a");
    for (let i = 0; i < ORPHAN_PASS_MAX_ATTEMPTS; i++) await settle(120_000);
    expect(captured.unadopted).toHaveLength(1);

    // Readable again: page C's first restore takes the released claim.
    restoreReadDown = false;
    await switchTo("page-c");
    expect(heldForAccount(9)).toHaveLength(1);
    // …and never what closed with A.
    expect(heldForAccount(10)).toEqual([]);
  });

  it("the unadopted-orphans notice is withdrawn once a restore adopts them", async () => {
    openRecords = [shellRecord(1, "page-a"), needsAccountRecord(9, "gone-page")];
    refusedDirs = new Set(["/w/1"]);
    await mountOn("page-a", ["page-a", "page-b", "page-c"]);
    await switchTo("page-b");
    restoreReadDown = true;
    await removePage("page-a");
    for (let i = 0; i < ORPHAN_PASS_MAX_ATTEMPTS; i++) await settle(120_000);
    expect(captured.unadopted).toEqual([{ removedPageIds: ["page-a"], sessionIds: null }]);

    restoreReadDown = false;
    await switchTo("page-c");
    expect(captured.unadopted).toEqual([]);
  });

  it("after a give-up, the next adopting restore leaves an orphan whose live tab was on the removed page", async () => {
    // Orphan 9's live tab is bound to A, which never reached it.
    openRecords = [shellRecord(1, "page-a"), resumableRecord(9, "gone-page")];
    refusedDirs = new Set(["/w/1"]);
    await mountOn("page-a", ["page-a", "page-b", "page-c"]);
    await switchTo("page-b");
    liveTerminals = [{ id: "old-9", pageId: "page-a" }];
    restoreReadDown = true;
    await removePage("page-a");
    for (let i = 0; i < ORPHAN_PASS_MAX_ATTEMPTS; i++) await settle(120_000);
    expect(captured.unadopted).toHaveLength(1);

    // Page C's first restore holds the released claim: orphan 9 stays put.
    restoreReadDown = false;
    await switchTo("page-c");
    await settle(5_000);
    expect(attemptedDirs).not.toContain("/w/9");
  });

  it("after a give-up, an adopting restore that cannot read the terminal list adopts no orphan until it can", async () => {
    openRecords = [shellRecord(1, "page-a"), resumableRecord(9, "gone-page")];
    refusedDirs = new Set(["/w/1"]);
    await mountOn("page-a", ["page-a", "page-b", "page-c"]);
    await switchTo("page-b");
    restoreReadDown = true;
    await removePage("page-a");
    for (let i = 0; i < ORPHAN_PASS_MAX_ATTEMPTS; i++) await settle(120_000);
    expect(captured.unadopted).toHaveLength(1);

    // Page C's first restore holds the released claim, but whether orphan 9's
    // live tab is on the removed page is unknown: it is left alone.
    restoreReadDown = false;
    terminalListDown = true;
    await switchTo("page-c");
    await settle(5_000);
    expect(attemptedDirs).not.toContain("/w/9");

    // …and is adopted once the list reads again.
    terminalListDown = false;
    await settle(GUARD_WAIT_INITIAL_MS);
    expect(spawnedDirs).toContain("/w/9");
  });

  it("a page this window holds is alive though a stale list drops it: its run completes and opens both gates", async () => {
    // A's run is mid-spawn on its own record 1, before orphan 9.
    openRecords = [shellRecord(1, "page-a"), needsAccountRecord(9, "gone-page")];
    holdDirs = new Set(["/w/1"]);
    await mountOn("page-a");
    expect(heldSpawns.has("/w/1")).toBe(true);

    // Another window holding a stale list writes it over this one's: A is
    // missing from the shared list, but this window still holds A.
    instanceStorage.setJSON(PAGES_STORAGE_KEY, [{ id: "page-b", name: "page-b", createdAt: 2 }]);
    await switchTo("page-b");

    await act(async () => heldSpawns.get("/w/1")!.resolve("ok"));
    await settle();
    await settle(5_000);
    // A's run went on: it adopted orphan 9 under the claim it kept…
    expect(heldForAccount(9)).toHaveLength(1);
    // …and opened its auto-save gate and its trim hold together.
    expect(captured.complete.has("page-a")).toBe(true);
    expect(trimSettled.get("page-a")).toBe(true);
  });

  it("a page this window holds keeps its guard retry though a stale list drops it", async () => {
    openRecords = [shellRecord(1, "page-a"), shellRecord(2, "page-a")];
    holdDirs = new Set(["/w/1"]);
    refusedDirs = new Set(["/w/2"]);
    await mountOn("page-a");
    instanceStorage.setJSON(PAGES_STORAGE_KEY, [{ id: "page-b", name: "page-b", createdAt: 2 }]);
    await switchTo("page-b");

    // A's run goes on to record 2, which the guard refuses: its retry arms.
    await act(async () => heldSpawns.get("/w/1")!.resolve("ok"));
    await settle();
    expect(waitFor("page-a")).toBeDefined();
    refusedDirs = new Set();
    await switchTo("page-a");
    await settle(GUARD_WAIT_INITIAL_MS);
    expect(spawnedDirs).toEqual(["/w/1", "/w/2"]);
  });

  it("an adoption pass whose terminal list is unreadable stays owed, and adopts once it reads", async () => {
    openRecords = [shellRecord(1, "page-a"), needsAccountRecord(9, "gone-page")];
    refusedDirs = new Set(["/w/1"]);
    await mountOn("page-a");
    await switchTo("page-b");

    terminalListDown = true;
    await removePage("page-a");
    expect(heldForAccount(9)).toEqual([]);

    // Readable again: the pass's retry adopts orphan 9.
    terminalListDown = false;
    await settle(GUARD_WAIT_INITIAL_MS);
    expect(heldForAccount(9)).toHaveLength(1);
  });

  it("a run whose settling throws still lets a later removal's adoption pass run", async () => {
    openRecords = [shellRecord(1, "page-a"), needsAccountRecord(9, "gone-page")];
    refusedDirs = new Set(["/w/1"]);
    vi.mocked(console.warn).mockImplementation((msg: unknown) => {
      if (String(msg).includes("the resource guard refused a restore on page page-a")) {
        throw new Error("settling failed");
      }
    });
    await mountOn("page-a");
    vi.mocked(console.warn).mockImplementation(() => {});
    expect(
      vi
        .mocked(console.error)
        .mock.calls.some((c) => String(c[0]).includes("settling the restore of page page-a")),
    ).toBe(true);
    await switchTo("page-b");

    await removePage("page-a");
    expect(heldForAccount(9)).toHaveLength(1);
  });

  it("an orphan whose live tab is bound to the removed page is not adopted by the page on screen", async () => {
    // A is refused at its own record 1 and never reaches orphan 9, whose live
    // tab another window had bound to A — a tab this window never noted.
    openRecords = [shellRecord(1, "page-a"), resumableRecord(9, "gone-page")];
    refusedDirs = new Set(["/w/1"]);
    await mountOn("page-a");
    await switchTo("page-b");
    liveTerminals = [{ id: "old-9", pageId: "page-a" }];
    const before = invokeCalls.length;

    await removePage("page-a");
    await settle(5_000);

    expect(spawnedDirs).not.toContain("/w/9");
    expect(attemptedDirs).not.toContain("/w/9");
    expect(
      invokeCalls.slice(before).filter(
        (
          c, // Reading the list, and the removal closing the page's tabs, are
        ) =>
          // not adoption.
          c.cmd !== "terminal_list" &&
          c.cmd !== "terminal_close" &&
          JSON.stringify(c.args ?? {}).includes("old-9"),
      ),
    ).toEqual([]);
  });
});
