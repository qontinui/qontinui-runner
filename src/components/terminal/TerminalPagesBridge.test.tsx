/**
 * `list-pages` — the terminal page roster served over the UI Bridge.
 *
 * The component is called as a plain function with `useUIComponent` mocked to
 * capture its registration, and the captured `list-pages` handler is invoked
 * directly: the assertions are on the exact rows an HTTP caller of
 * `POST /ui-bridge/control/component/terminal-pages/action/list-pages` gets.
 * Expected values are written out as literals, never derived from the input
 * fixtures, so a renamed or dropped field turns these red.
 */

import { describe, it, expect, vi, beforeEach } from "vitest";

const { registrations, currentWindow } = vi.hoisted(() => ({
  // What the mocked `getCurrentWindow()` does: answer this label, or throw
  // (a non-Tauri environment) when it is null.
  currentWindow: { label: "main" as string | null },
  registrations: [] as Array<{
    id: string;
    actions: Array<{ id: string; effect: string; handler: (params?: unknown) => unknown }>;
  }>,
}));

vi.mock("@qontinui/ui-bridge", () => ({
  useUIComponent: (opts: (typeof registrations)[number]) => {
    registrations.push(opts);
  },
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => {
    if (currentWindow.label === null) throw new Error("not running under Tauri");
    return { label: currentWindow.label };
  },
}));

const { TerminalPagesBridge, isMainWindow, shouldServePageRoster } =
  await import("./TerminalPagesBridge");

function listPages(props: Parameters<typeof TerminalPagesBridge>[0], params?: unknown): unknown {
  registrations.length = 0;
  TerminalPagesBridge(props);
  expect(registrations).toHaveLength(1);
  const reg = registrations[0];
  expect(reg.id).toBe("terminal-pages");
  const action = reg.actions.find((a) => a.id === "list-pages");
  expect(action).toBeDefined();
  expect(action!.effect).toBe("read");
  return action!.handler(params);
}

describe("TerminalPagesBridge list-pages", () => {
  beforeEach(() => {
    registrations.length = 0;
  });

  it("returns every page in tab order with its literal name", () => {
    const rows = listPages({
      allPages: [
        { id: "default", name: "Terminal", createdAt: 0 },
        { id: "page-7", name: "coord merge train", createdAt: 1 },
      ],
      boundPages: {},
      activePageId: "page-7",
    });
    expect(rows).toEqual([
      { pageId: "default", name: "Terminal", active: false, window: "main" },
      { pageId: "page-7", name: "coord merge train", active: true, window: "main" },
    ]);
  });

  it("still lists a page detached into a pop-out, naming that window", () => {
    const rows = listPages({
      allPages: [
        { id: "default", name: "Terminal", createdAt: 0 },
        { id: "p-a", name: "runner fixes", createdAt: 1 },
        { id: "p-b", name: "web review", createdAt: 2 },
      ],
      boundPages: { "p-a": "terminal-popout-3" },
      activePageId: "default",
    });
    expect(rows).toEqual([
      { pageId: "default", name: "Terminal", active: true, window: "main" },
      { pageId: "p-a", name: "runner fixes", active: false, window: "terminal-popout-3" },
      { pageId: "p-b", name: "web review", active: false, window: "main" },
    ]);
  });

  it("refuses an undeclared argument rather than answering over it", () => {
    expect(() =>
      listPages(
        {
          allPages: [{ id: "default", name: "Terminal", createdAt: 0 }],
          boundPages: {},
          activePageId: "default",
        },
        { zzz: "x" },
      ),
    ).toThrow(/ACTION_PARAMS_INVALID/);
  });
});

describe("isMainWindow (gates where list-pages is registered)", () => {
  beforeEach(() => {
    currentWindow.label = "main";
  });

  it('is true in the window labelled "main"', () => {
    currentWindow.label = "main";
    expect(isMainWindow()).toBe(true);
  });

  it("is false in a pop-out window", () => {
    currentWindow.label = "terminal-popout-3";
    expect(isMainWindow()).toBe(false);
  });

  it("is true when getCurrentWindow throws (no Tauri: the one window is main)", () => {
    currentWindow.label = null;
    expect(isMainWindow()).toBe(true);
  });
});

describe("shouldServePageRoster (the App mount gate for list-pages)", () => {
  it("serves in the unpinned main window", () => {
    expect(shouldServePageRoster(false, "main")).toBe(true);
  });

  it("does not serve in a pop-out window", () => {
    expect(shouldServePageRoster(false, "terminal-popout-3")).toBe(false);
  });

  it("does not serve in a pinned window", () => {
    expect(shouldServePageRoster(true, "main")).toBe(false);
  });

  it("reads this window's label when none is passed", () => {
    currentWindow.label = "terminal-popout-3";
    expect(shouldServePageRoster(false)).toBe(false);
    currentWindow.label = "main";
    expect(shouldServePageRoster(false)).toBe(true);
  });
});
