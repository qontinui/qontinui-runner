// @vitest-environment jsdom
/**
 * The auto-prune of empty auto-created pages and the one-time roster repair,
 * through the mounted hook (plan 2026-11-05 Phase 6). The seam is the page
 * `reconcile` that runs on mount and on every `terminal-created` /
 * `terminal-exit`. The backend serves the 2026-11-05 merytshost roster
 * FIXTURE. Measured (coord finding `8c9e1419`): the totals (40 persisted
 * pages, 22 occupied), the two "Terminal" pages, the duplicate names, the six
 * operator names, `3e86a8ae`'s creation time, the 20:51:51Z batch, and that
 * "Page 19" onward is populated. Reconstructed: every other id, the October
 * names below "Page 19", every other `createdAt`, and the exact 22-page
 * occupied set (see `fixtures/terminalPageRoster.ts`).
 */

import { act, createElement, useLayoutEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { invokeMock, listeners, windowLabel } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
  listeners: new Map<string, () => void>(),
  windowLabel: { current: "main" },
}));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ label: windowLabel.current }),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((event: string, cb: () => void) => {
    listeners.set(event, cb);
    return Promise.resolve(() => {});
  }),
}));

import { instanceStorage } from "@/lib/instance-storage";
import { setPageBindings } from "@/lib/pageBindings";
import { __resetPerfCapsForTest } from "@/lib/perfCaps";
import {
  OCCUPIED_2026_11_05,
  OPERATOR_PAGE_NAMES,
  ROSTER_2026_11_05,
} from "./fixtures/terminalPageRoster";
import { TerminalPageTabBar } from "./TerminalPageTabBar";
import { PAGES_STORAGE_KEY, useTerminalPages, type TerminalPageConfig } from "./useTerminalPages";

const NOW = Date.UTC(2026, 10, 5, 12);
const ACTIVE_PAGE_KEY = "qontinui-terminal-active-page";
/** The one-time repair's version key (D2), spelled as a literal so the test pins it. */
const PAGES_VERSION_KEY = "qontinui-terminal-pages-version";

/** The twelve empty `auto` pages in the fixture (its other six empties are operator pages). */
const EMPTY_AUTO_IDS = [
  "oct-page-2",
  "oct-page-3",
  "oct-page-9",
  "oct-page-11",
  "3e86a8ae",
  "oct-page-14",
  "oct-page-17",
  "oct-page-20",
  "oct-page-21",
  "oct-page-22",
  "oct-page-23a",
  "oct-page-23b",
];

interface Backend {
  /** Page ids of live terminals, per `terminal_list` call (the last entry repeats). */
  terminals: string[][];
  sessions: string[];
  deferredByDrain?: boolean;
  terminalListThrows?: boolean;
  sweepSettled?: boolean;
  perf?: Record<string, unknown> | "throws";
  /** Restorable page ids for every session-list call AFTER the first. */
  sessionsLater?: string[];
  /**
   * The `terminal_list` call `removeEmptyPage` makes (the one right after a
   * session-list read): throw it, or run a side effect first.
   */
  removalListThrows?: boolean;
  onRemovalList?: () => void;
}

let backend: Backend;
let terminalListCalls = 0;
let sessionListCalls = 0;
let lastCmd = "";

function serve() {
  invokeMock.mockImplementation(async (cmd: string) => {
    const prev = lastCmd;
    lastCmd = cmd;
    if (cmd === "terminal_list") {
      if (backend.terminalListThrows) throw new Error("no terminal_list");
      if (prev === "terminal_session_list_open") {
        backend.onRemovalList?.();
        if (backend.removalListThrows) throw new Error("no terminal_list at removal");
      }
      const t = backend.terminals[Math.min(terminalListCalls++, backend.terminals.length - 1)];
      return {
        success: true,
        data: { terminals: t.map((pageId, i) => ({ id: `t-${pageId}-${i}`, pageId })) },
      };
    }
    if (cmd === "terminal_session_list_open") {
      const sessions =
        sessionListCalls++ > 0 && backend.sessionsLater ? backend.sessionsLater : backend.sessions;
      return backend.deferredByDrain
        ? {
            success: true,
            data: { sessions: [], deferredByDrain: { state: "drained", reason: "drained" } },
          }
        : { success: true, data: { sessions: sessions.map((pageId) => ({ pageId })) } };
    }
    if (cmd === "terminal_holder_sweep_settled") {
      return { success: true, data: { settled: backend.sweepSettled ?? true } };
    }
    if (cmd === "get_performance_settings") {
      if (backend.perf === "throws") throw new Error("no settings");
      return backend.perf ?? {};
    }
    throw new Error(`unexpected ${cmd}`);
  });
}

let container: HTMLDivElement;
let latestHook: (() => Hook) | null = null;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.spyOn(Date, "now").mockReturnValue(NOW);
  localStorage.clear();
  setPageBindings({});
  __resetPerfCapsForTest();
  listeners.clear();
  terminalListCalls = 0;
  sessionListCalls = 0;
  lastCmd = "";
  // Live terminals on the 21 occupied non-default pages; one restorable row on default.
  backend = {
    terminals: [[...OCCUPIED_2026_11_05].filter((id) => id !== "default")],
    sessions: ["default"],
  };
  serve();
  instanceStorage.setJSON(PAGES_STORAGE_KEY, ROSTER_2026_11_05);
  instanceStorage.setItem(ACTIVE_PAGE_KEY, "restored-1");
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  localStorage.clear();
  invokeMock.mockReset();
  vi.restoreAllMocks();
});

type Hook = ReturnType<typeof useTerminalPages>;

/** Let every awaited invoke in the reconcile + prune pass resolve. */
async function settle() {
  for (let i = 0; i < 5; i++) {
    await act(() => new Promise((r) => setTimeout(r, 0)));
  }
}

async function mount(): Promise<() => Hook> {
  let latest: Hook | null = null;
  function Probe() {
    latest = useTerminalPages();
    return null;
  }
  // Set before the first render: the prune pass can run inside it.
  latestHook = () => latest!;
  await act(async () => root.render(createElement(Probe)));
  await settle();
  return latestHook;
}

/** Fire a debounced reconcile the way a `terminal-exit` does, and let it run. */
async function terminalExit() {
  await act(async () => listeners.get("terminal-exit")!());
  await act(() => new Promise((r) => setTimeout(r, 450)));
  await settle();
}

const POPULATED_NAMES = Array.from({ length: 21 }, (_, i) => `Page ${19 + i}`);
const populatedNames = (pages: TerminalPageConfig[]) =>
  names(pages.filter((p) => /^(restored|later)-/.test(p.id)));

const persisted = () => instanceStorage.getJSON<TerminalPageConfig[]>(PAGES_STORAGE_KEY, []);

/** Whether raw `localStorage` key `raw` is instance key `key` (bare, or `<key>:<port>`). */
const isKey = (raw: string, key: string) => raw === key || raw.startsWith(`${key}:`);

/** Make `localStorage.setItem` throw a full-storage error for instance key `key`. */
function failWritesTo(key: string) {
  const realSetItem = Storage.prototype.setItem;
  vi.spyOn(Storage.prototype, "setItem").mockImplementation(function (
    this: Storage,
    raw: string,
    value: string,
  ) {
    if (isKey(raw, key)) throw new DOMException("storage is full", "QuotaExceededError");
    realSetItem.call(this, raw, value);
  });
}
const ids = (pages: TerminalPageConfig[]) => pages.map((p) => p.id);
const names = (pages: TerminalPageConfig[]) => pages.map((p) => p.name);

describe("the auto-prune over the 2026-11-05 roster", () => {
  it("leaves default, the operator pages and the populated pages, with no duplicate names", async () => {
    const hook = await mount();
    const pages = hook().allPages;

    expect(new Set(names(pages)).size).toBe(pages.length);
    expect(pages).toHaveLength(28);
    expect(ids(pages)).not.toContain("3e86a8ae");
    for (const id of EMPTY_AUTO_IDS) expect(ids(pages)).not.toContain(id);
    expect(ids(pages)).toContain("default");
    for (const name of OPERATOR_PAGE_NAMES) expect(names(pages)).toContain(name);
    // The populated Page 19 onward keep the names the operator has been reading.
    expect(populatedNames(pages)).toEqual(POPULATED_NAMES);

    expect(ids(persisted())).toEqual(ids(pages));
    expect(instanceStorage.getItem(PAGES_VERSION_KEY)).toBe("2");
    expect(hook().pruneNotice?.removed.map((r) => r.page.id)).toEqual(EMPTY_AUTO_IDS);
    expect(hook().pruneNotice?.renames).toEqual([]);
  });

  it("closes no PTY: the removal is record-only", async () => {
    await mount();
    expect(invokeMock).not.toHaveBeenCalledWith("terminal_close", expect.anything());
  });

  it("drops a pruned page's page:<id>: keys, and Undo puts them back", async () => {
    instanceStorage.setItem("page:oct-page-9:qontinui-zone-layout", "layout-9");
    const hook = await mount();
    expect(instanceStorage.getItem("page:oct-page-9:qontinui-zone-layout")).toBeNull();
    act(() => hook().undoPrune());
    expect(instanceStorage.getItem("page:oct-page-9:qontinui-zone-layout")).toBe("layout-9");
  });

  it("names the removal in the strip with an Undo", async () => {
    const hook = await mount();
    const html = renderToStaticMarkup(
      <TerminalPageTabBar
        pages={hook().visiblePages}
        activePageId={hook().activePageId}
        onSelectPage={() => {}}
        onAddPage={() => {}}
        onRemovePage={() => {}}
        onRenamePage={() => {}}
        pruneNotice={hook().pruneNotice}
        onUndoPrune={() => {}}
      />,
    );
    expect(html).toContain("Removed 12 empty auto-created pages");
    expect(html).toContain(">Undo</button>");
  });
});

describe("Undo", () => {
  it("a second prune inside the Undo window extends the notice, and Undo restores both", async () => {
    // A page that turns PRUNE_MIN_AGE_MS old one second after mount.
    const young = { id: "young", name: "Page 40", createdAt: NOW - 299_000, origin: "auto" };
    instanceStorage.setJSON(PAGES_STORAGE_KEY, [...ROSTER_2026_11_05, young]);
    const hook = await mount();
    expect(hook().pruneNotice?.removed).toHaveLength(12);
    vi.spyOn(Date, "now").mockReturnValue(NOW + 2_000);
    await terminalExit();
    expect(ids(hook().allPages)).not.toContain("young");
    expect(hook().pruneNotice?.removed).toHaveLength(13);
    act(() => hook().undoPrune());
    expect(ids(hook().allPages)).toEqual([...ids([...ROSTER_2026_11_05]), "young"]);
  });

  it("restores the order, and the next reconcile prunes nothing", async () => {
    const hook = await mount();
    act(() => hook().undoPrune());

    expect(ids(hook().allPages)).toEqual(ids([...ROSTER_2026_11_05]));
    expect(ids(persisted())).toEqual(ids([...ROSTER_2026_11_05]));
    expect(hook().pruneNotice).toBeNull();
    // Restored as operator pages, renamed where their name is now taken.
    const back = hook().allPages.filter((p) => EMPTY_AUTO_IDS.includes(p.id));
    expect(back.every((p) => p.origin === "operator")).toBe(true);
    expect(new Set(names(hook().allPages)).size).toBe(ROSTER_2026_11_05.length);

    await terminalExit();
    expect(ids(hook().allPages)).toEqual(ids([...ROSTER_2026_11_05]));
  });
});

describe("a pop-out window opened before the repair", () => {
  /** Mount the hook as an unpinned pop-out (`term-2`), in its own root. */
  async function mountPopOut(): Promise<{ hook: () => Hook; unmount: () => void }> {
    const popContainer = document.createElement("div");
    document.body.appendChild(popContainer);
    const popRoot = createRoot(popContainer);
    let latest: Hook | null = null;
    function PopOut() {
      latest = useTerminalPages();
      return null;
    }
    windowLabel.current = "term-2";
    try {
      await act(async () => popRoot.render(createElement(PopOut)));
      await settle();
    } finally {
      windowLabel.current = "main";
    }
    return {
      hook: () => latest!,
      unmount: () => {
        act(() => popRoot.unmount());
        popContainer.remove();
      },
    };
  }

  it("does not write the repaired duplicate names back", async () => {
    backend.perf = { prune_empty_auto_pages: false };
    // The pop-out loads the roster while it still has its duplicates.
    const popOut = await mountPopOut();
    try {
      expect(popOut.hook().allPages.find((p) => p.id === "3e86a8ae")?.name).toBe("Terminal");
      await mount();
      expect(instanceStorage.getItem(PAGES_VERSION_KEY)).toBe("2");
      expect(persisted().find((p) => p.id === "3e86a8ae")?.name).toBe("Page 40");

      // Any write from the pop-out, e.g. its "+".
      act(() => void popOut.hook().addPage());

      const roster = persisted();
      expect(roster).toHaveLength(41);
      expect(new Set(names(roster)).size).toBe(41);
      expect(roster.find((p) => p.id === "3e86a8ae")?.name).toBe("Page 40");
      expect(roster.filter((p) => p.name === "Terminal").map((p) => p.id)).toEqual(["default"]);
      expect(roster.at(-1)?.name).toBe("Page 46");
      // The pop-out now shows the repaired names too.
      expect(popOut.hook().allPages.find((p) => p.id === "3e86a8ae")?.name).toBe("Page 40");
    } finally {
      popOut.unmount();
    }
  });

  it("keeps a page the pop-out renamed, and a page it added, through the main window's prune", async () => {
    backend.sweepSettled = false;
    const popOut = await mountPopOut();
    try {
      // Mounted last, so the `terminal-exit` listener is the main window's.
      const main = await mount();
      expect(popOut.hook().renamePage("oct-page-2", "mine")).toBe(true);
      let added = "";
      act(() => void (added = popOut.hook().addPage()));
      // The main window's copy has caught up with neither.
      expect(main().allPages.find((p) => p.id === "oct-page-2")?.origin).toBe("auto");

      backend.sweepSettled = true;
      await terminalExit();

      const roster = persisted();
      expect(roster.find((p) => p.id === "oct-page-2")).toMatchObject({
        name: "mine",
        origin: "operator",
      });
      expect(ids(roster)).toContain(added);
      expect(main().pruneNotice?.removed.map((r) => r.page.id)).toEqual(
        EMPTY_AUTO_IDS.filter((id) => id !== "oct-page-2"),
      );
      // The main window's writes were built on the store, so it now shows both.
      expect(main().allPages.find((p) => p.id === "oct-page-2")?.name).toBe("mine");
      expect(ids(main().allPages)).toContain(added);
    } finally {
      popOut.unmount();
    }
  });

  it("does not put back the pages the prune removed", async () => {
    const popOut = await mountPopOut();
    try {
      expect(popOut.hook().allPages).toHaveLength(40);
      await mount();
      expect(persisted()).toHaveLength(28);

      act(() => void popOut.hook().addPage());

      const roster = persisted();
      expect(roster).toHaveLength(29);
      for (const id of EMPTY_AUTO_IDS) expect(ids(roster)).not.toContain(id);
      expect(new Set(names(roster)).size).toBe(29);
      expect(roster.at(-1)?.name).toBe("Page 40");
    } finally {
      popOut.unmount();
    }
  });
});

describe("the repair's version", () => {
  it("is not recorded when the page-list save failed, so the repair runs again", async () => {
    backend.perf = { prune_empty_auto_pages: false };
    failWritesTo(PAGES_STORAGE_KEY);
    const hook = await mount();
    // Renamed in memory, but the store still holds the duplicates.
    expect(hook().allPages.find((p) => p.id === "3e86a8ae")?.name).toBe("Page 40");
    expect(persisted().find((p) => p.id === "3e86a8ae")?.name).toBe("Terminal");
    expect(instanceStorage.getItem(PAGES_VERSION_KEY)).toBeNull();

    // The store recovers; the next mount repairs and records.
    vi.mocked(Storage.prototype.setItem).mockRestore();
    act(() => root.unmount());
    root = createRoot(container);
    await mount();
    expect(persisted().find((p) => p.id === "3e86a8ae")?.name).toBe("Page 40");
    expect(new Set(names(persisted())).size).toBe(40);
    expect(instanceStorage.getItem(PAGES_VERSION_KEY)).toBe("2");
  });

  it("ranks the repair's claims on a fresh occupancy read, not the reconcile's", async () => {
    // By the repair's own read a restorable row has landed on the October
    // "Page 20" too, so both holders are occupied and the older keeps the name.
    // On the reconcile's read only the populated one was.
    backend.perf = { prune_empty_auto_pages: false };
    backend.sessionsLater = ["default", "oct-page-20"];
    const hook = await mount();
    const byId = (id: string) => hook().allPages.find((p) => p.id === id)?.name;
    expect(byId("oct-page-20")).toBe("Page 20");
    expect(byId("restored-2")).toBe("Page 41");
  });

  it("is not recorded, and nothing throws, when the version write itself fails", async () => {
    backend.perf = { prune_empty_auto_pages: false };
    failWritesTo(PAGES_VERSION_KEY);
    const hook = await mount();
    expect(hook().allPages.find((p) => p.id === "3e86a8ae")?.name).toBe("Page 40");
    expect(persisted().find((p) => p.id === "3e86a8ae")?.name).toBe("Page 40");
    expect(instanceStorage.getItem(PAGES_VERSION_KEY)).toBeNull();
    expect(hook().pruneNotice?.renames.map((r) => r.id)).toContain("3e86a8ae");
  });
});

describe("what the prune never does", () => {
  it("prunes nothing on a drain-deferred read, but still mints tabs from what it read", async () => {
    backend.deferredByDrain = true;
    backend.terminals = [[...backend.terminals[0], "fresh-page"]];
    const hook = await mount();

    expect(ids(hook().allPages)).toEqual([...ids([...ROSTER_2026_11_05]), "fresh-page"]);
    expect(instanceStorage.getItem(PAGES_VERSION_KEY)).toBeNull();
    expect(invokeMock).not.toHaveBeenCalledWith("terminal_holder_sweep_settled");
  });

  it("keeps a page a terminal lands on between the read and the removal", async () => {
    // The reconcile's read sees the measured occupancy; every later read also
    // lists a terminal on oct-page-2.
    backend.terminals = [backend.terminals[0], [...backend.terminals[0], "oct-page-2"]];
    const hook = await mount();
    expect(ids(hook().allPages)).toContain("oct-page-2");
    expect(ids(hook().allPages)).not.toContain("oct-page-3");
  });

  it("prunes nothing with the off-switch set, and still repairs names once", async () => {
    backend.perf = { prune_empty_auto_pages: false };
    const hook = await mount();
    expect(hook().allPages).toHaveLength(40);
    // The repair: 3e86a8ae "Terminal" and the October duplicates are renamed.
    expect(new Set(names(hook().allPages)).size).toBe(40);
    expect(hook().allPages.find((p) => p.id === "3e86a8ae")?.name).toBe("Page 40");
    // The populated pages keep their names; the empty October duplicates move.
    expect(populatedNames(hook().allPages)).toEqual(POPULATED_NAMES);
    expect(hook().allPages.find((p) => p.id === "oct-page-20")?.name).toBe("Page 41");
    expect(instanceStorage.getItem(PAGES_VERSION_KEY)).toBe("2");
  });

  it("prunes and repairs nothing in an unpinned pop-out window", async () => {
    windowLabel.current = "term-2";
    try {
      const hook = await mount();
      expect(hook().allPages).toHaveLength(40);
      expect(ids(hook().allPages)).toContain("3e86a8ae");
      expect(hook().allPages.find((p) => p.id === "3e86a8ae")?.name).toBe("Terminal");
      expect(instanceStorage.getItem(PAGES_VERSION_KEY)).toBeNull();
    } finally {
      windowLabel.current = "main";
    }
  });

  it("prunes nothing while the off-switch is unread", async () => {
    backend.perf = "throws";
    const hook = await mount();
    expect(hook().allPages).toHaveLength(40);
    expect(instanceStorage.getItem(PAGES_VERSION_KEY)).toBeNull();
  });

  it("prunes nothing until the holder boot sweep has settled", async () => {
    backend.sweepSettled = false;
    const hook = await mount();
    expect(hook().allPages).toHaveLength(40);
    expect(instanceStorage.getItem(PAGES_VERSION_KEY)).toBeNull();
  });

  it("never prunes the active page or a page bound to a pop-out", async () => {
    instanceStorage.setItem(ACTIVE_PAGE_KEY, "oct-page-2");
    setPageBindings({ "oct-page-3": "terminal-popout-1" });
    const hook = await mount();
    expect(ids(hook().allPages)).toContain("oct-page-2");
    expect(ids(hook().allPages)).toContain("oct-page-3");
    expect(ids(hook().allPages)).not.toContain("oct-page-9");
  });

  it("an empty October duplicate that survives as the active page does not take a populated name", async () => {
    instanceStorage.setItem(ACTIVE_PAGE_KEY, "oct-page-20");
    const hook = await mount();
    expect(populatedNames(hook().allPages)).toEqual(POPULATED_NAMES);
    expect(hook().allPages.find((p) => p.id === "oct-page-20")?.name).toBe("Page 40");
    expect(new Set(names(hook().allPages)).size).toBe(hook().allPages.length);
  });

  it("re-checks the guards at removal time: a page activated mid-pass is kept", async () => {
    let fired = false;
    backend.onRemovalList = () => {
      if (fired) return;
      fired = true;
      latestHook!().setActivePageId("oct-page-23b");
    };
    const hook = await mount();
    expect(ids(hook().allPages)).toContain("oct-page-23b");
    expect(ids(hook().allPages)).not.toContain("oct-page-23a");
  });

  it("re-checks the guards at removal time: a page renamed mid-pass is kept", async () => {
    let fired = false;
    backend.onRemovalList = () => {
      if (fired) return;
      fired = true;
      expect(latestHook!().renamePage("oct-page-23b", "keepers")).toBe(true);
    };
    const hook = await mount();
    expect(hook().allPages.find((p) => p.id === "oct-page-23b")).toMatchObject({
      name: "keepers",
      origin: "operator",
    });
    expect(ids(hook().allPages)).not.toContain("oct-page-23a");
  });

  it("still prunes an auto page whose name editor was opened and closed unchanged", async () => {
    // The tab bar commits a rename on blur. Keeping the name is not a rename,
    // so the page stays `auto` and the prune still takes it.
    backend.sweepSettled = false;
    const hook = await mount();
    expect(hook().renamePage("oct-page-2", "Page 2")).toBe(true);
    expect(hook().renamePage("oct-page-3", " Page 3 ")).toBe(true);
    expect(hook().allPages.find((p) => p.id === "oct-page-2")?.origin).toBe("auto");

    backend.sweepSettled = true;
    await terminalExit();
    expect(ids(hook().allPages)).not.toContain("oct-page-2");
    expect(ids(hook().allPages)).not.toContain("oct-page-3");
    expect(hook().pruneNotice?.removed.map((r) => r.page.id)).toEqual(EMPTY_AUTO_IDS);
  });

  it("re-checks the guards at removal time: a page popped out mid-pass is kept", async () => {
    let fired = false;
    backend.onRemovalList = () => {
      if (fired) return;
      fired = true;
      setPageBindings({ "oct-page-23b": "terminal-popout-1" });
    };
    const hook = await mount();
    expect(ids(hook().allPages)).toContain("oct-page-23b");
    expect(ids(hook().allPages)).not.toContain("oct-page-23a");
  });

  it("keeps every page when the removal-time terminal list cannot be read", async () => {
    backend.removalListThrows = true;
    const hook = await mount();
    expect(hook().allPages).toHaveLength(40);
    expect(hook().pruneNotice?.removed).toEqual([]);
  });

  it("keeps a page whose re-read shows only a restorable record on it", async () => {
    backend.sessionsLater = ["default", "oct-page-11"];
    const hook = await mount();
    expect(ids(hook().allPages)).toContain("oct-page-11");
    expect(ids(hook().allPages)).not.toContain("oct-page-9");
  });

  it("writes no version, and renames nothing, after a pass that met an incomplete re-read", async () => {
    // Every session-list read after the reconcile's is drain-deferred.
    let calls = 0;
    const base = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation(async (cmd: string, ...rest: unknown[]) => {
      if (cmd === "terminal_session_list_open" && calls++ > 0) {
        return { success: true, data: { sessions: [], deferredByDrain: { state: "drained" } } };
      }
      return base(cmd, ...rest);
    });
    const hook = await mount();
    expect(hook().allPages).toHaveLength(40);
    expect(instanceStorage.getItem(PAGES_VERSION_KEY)).toBeNull();
    expect(hook().allPages.find((p) => p.id === "3e86a8ae")?.name).toBe("Terminal");
  });

  // Each writer below is dispatched while the prune is removing "oct-page-2"
  // ("Page 2"), before that removal is queued, so the writer's updater runs on
  // a roster that still holds "oct-page-2". A writer that copied that roster
  // into the ref brought "oct-page-2" back until the next passive effect: a
  // rename to "Page 2" made in that window (probed from a layout effect, which
  // runs first) was refused for a name no page holds.
  it.each<[string, (h: Hook) => void]>([
    [
      // "tmux 2" is free in the ref and taken by the time the rename renders.
      "a refused rename",
      (h) => {
        h.addPage("tmux");
        expect(h.renamePage("operator-1", "tmux 2")).toBe(true);
      },
    ],
    ["a rename", (h) => expect(h.renamePage("operator-1", "keepers")).toBe(true)],
    ["a reorder", (h) => h.reorderPage("operator-2", "operator-1")],
    ["a page default cwd", (h) => h.setPageDefaultWorkingDir("operator-2", "/repo")],
    [
      "a project page",
      (h) => void h.ensureProjectPage({ boundPageId: "proj-1", projectName: "proj" }),
    ],
  ])("%s mid-pass does not bring back a page the prune removed", async (_, write) => {
    let fired = false;
    let probeDue = false;
    let probe: boolean | null = null;
    backend.onRemovalList = () => {
      if (fired) return;
      fired = true;
      write(latestHook!());
      probeDue = true;
    };
    let latest: Hook | null = null;
    function Probe() {
      latest = useTerminalPages();
      useLayoutEffect(() => {
        if (!probeDue) return;
        probeDue = false;
        probe = latest!.renamePage("operator-3", "Page 2");
      });
      return null;
    }
    latestHook = () => latest!;
    await act(async () => root.render(createElement(Probe)));
    await settle();

    expect(probe).toBe(true);
    expect(latest!.allPages.find((p) => p.id === "operator-3")?.name).toBe("Page 2");
    expect(ids(latest!.allPages)).not.toContain("oct-page-2");
  });

  it("writes no version after an incomplete read", async () => {
    backend.terminalListThrows = true;
    const hook = await mount();
    expect(hook().allPages).toHaveLength(40);
    expect(instanceStorage.getItem(PAGES_VERSION_KEY)).toBeNull();
  });
});
