// @vitest-environment jsdom
/**
 * The auto-prune of empty auto-created pages and the one-time roster repair,
 * through the mounted hook (plan 2026-11-05 Phase 6). The seam is the page
 * `reconcile` that runs on mount and on every `terminal-created` /
 * `terminal-exit`; the backend is the measured 2026-11-05 merytshost roster
 * (40 persisted pages, 22 occupied).
 */

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { invokeMock, listeners } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
  listeners: new Map<string, () => void>(),
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
import { PAGES_VERSION_KEY } from "./pagePrune";
import { TerminalPageTabBar } from "./TerminalPageTabBar";
import { PAGES_STORAGE_KEY, useTerminalPages, type TerminalPageConfig } from "./useTerminalPages";

const NOW = Date.UTC(2026, 10, 5, 12);
const ACTIVE_PAGE_KEY = "qontinui-terminal-active-page";

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
}

let backend: Backend;
let terminalListCalls = 0;

function serve() {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "terminal_list") {
      if (backend.terminalListThrows) throw new Error("no terminal_list");
      const t = backend.terminals[Math.min(terminalListCalls++, backend.terminals.length - 1)];
      return {
        success: true,
        data: { terminals: t.map((pageId, i) => ({ id: `t-${pageId}-${i}`, pageId })) },
      };
    }
    if (cmd === "terminal_session_list_open") {
      return backend.deferredByDrain
        ? {
            success: true,
            data: { sessions: [], deferredByDrain: { state: "drained", reason: "drained" } },
          }
        : { success: true, data: { sessions: backend.sessions.map((pageId) => ({ pageId })) } };
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
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.spyOn(Date, "now").mockReturnValue(NOW);
  localStorage.clear();
  setPageBindings({});
  __resetPerfCapsForTest();
  listeners.clear();
  terminalListCalls = 0;
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
  await act(async () => root.render(createElement(Probe)));
  await settle();
  return () => latest!;
}

/** Fire a debounced reconcile the way a `terminal-exit` does, and let it run. */
async function terminalExit() {
  await act(async () => listeners.get("terminal-exit")!());
  await act(() => new Promise((r) => setTimeout(r, 450)));
  await settle();
}

const persisted = () => instanceStorage.getJSON<TerminalPageConfig[]>(PAGES_STORAGE_KEY, []);
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
    const populated = pages.filter((p) => /^(restored|later)-/.test(p.id));
    expect(names(populated)).toEqual(Array.from({ length: 21 }, (_, i) => `Page ${19 + i}`));

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
    expect(instanceStorage.getItem(PAGES_VERSION_KEY)).toBe("2");
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

  it("writes no version after an incomplete read", async () => {
    backend.terminalListThrows = true;
    const hook = await mount();
    expect(hook().allPages).toHaveLength(40);
    expect(instanceStorage.getItem(PAGES_VERSION_KEY)).toBeNull();
  });
});
