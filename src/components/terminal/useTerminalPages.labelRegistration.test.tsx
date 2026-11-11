// @vitest-environment jsdom
/**
 * Tab-label registration through the mounted hook (plan
 * 2026-11-08-save-per-tab-runner-session-lists-on-sessions-page-kept-14-days,
 * Phase 1, D2). The main window sends its WHOLE page list, pages detached into
 * a pop-out included, in tab order, to the runner backend once per change of
 * that list: a rename, a reorder, and a page `reconcile` synthesizes all reach
 * it through one effect, and a page-pinned pop-out sends nothing. Expected
 * payloads are written out as literals.
 */

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { invokeMock, backend } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
  // The page ids the mocked backend reports a live terminal on.
  backend: { terminalPageIds: [] as string[] },
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));

import { instanceStorage } from "@/lib/instance-storage";
import { setPageBindings } from "@/lib/pageBindings";
import {
  PAGES_STORAGE_KEY,
  REGISTER_PAGE_LABELS_COMMAND,
  pageLabelRegistration,
  useTerminalPages,
  type PageLabelRegistration,
  type TerminalPageConfig,
} from "./useTerminalPages";

let container: HTMLDivElement;
let root: Root;

const ROSTER: TerminalPageConfig[] = [
  { id: "p-alpha", name: "Alpha", createdAt: 1 },
  { id: "p-beta", name: "Beta", createdAt: 2 },
  { id: "p-popped", name: "Popped", createdAt: 3 },
];

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  localStorage.clear();
  backend.terminalPageIds = [];
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "terminal_list") {
      return {
        success: true,
        data: { terminals: backend.terminalPageIds.map((pageId, i) => ({ id: `t${i}`, pageId })) },
      };
    }
    if (cmd === "terminal_session_list_open") return { data: { sessions: [] } };
    if (cmd === REGISTER_PAGE_LABELS_COMMAND) return { success: true };
    throw new Error(`unexpected ${cmd}`);
  });
  instanceStorage.setJSON(PAGES_STORAGE_KEY, ROSTER);
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  localStorage.clear();
  invokeMock.mockReset();
  window.history.replaceState(null, "", "/");
});

type Hook = ReturnType<typeof useTerminalPages>;

/** Mount the hook and let its mount-time reconcile settle. */
async function mount(): Promise<() => Hook> {
  let latest: Hook | null = null;
  function Probe() {
    latest = useTerminalPages();
    return null;
  }
  await act(async () => {
    root.render(createElement(Probe));
  });
  await act(async () => {});
  return () => latest!;
}

/** Every page list the hook registered, oldest first. */
function registrations(): PageLabelRegistration[][] {
  return invokeMock.mock.calls
    .filter(([cmd]) => cmd === REGISTER_PAGE_LABELS_COMMAND)
    .map(([, args]) => (args as { pages: PageLabelRegistration[] }).pages);
}

describe("pageLabelRegistration", () => {
  it("numbers every page by its tab position", () => {
    expect(
      pageLabelRegistration([
        { id: "default", name: "Terminal", createdAt: 0 },
        { id: "p-1", name: "Work", createdAt: 1 },
      ]),
    ).toEqual({
      pages: [
        { pageId: "default", label: "Terminal", order: 0 },
        { pageId: "p-1", label: "Work", order: 1 },
      ],
    });
  });
});

describe("the main window registers its page list", () => {
  it("on mount: the whole roster in tab order, a page detached into a pop-out included", async () => {
    setPageBindings({ "p-popped": "terminal-popout-1" });
    await mount();
    expect(registrations()).toEqual([
      [
        { pageId: "p-alpha", label: "Alpha", order: 0 },
        { pageId: "p-beta", label: "Beta", order: 1 },
        { pageId: "p-popped", label: "Popped", order: 2 },
      ],
    ]);
  });

  it("once more after a rename, with the new label", async () => {
    const hook = await mount();
    const before = registrations().length;
    await act(async () => {
      expect(hook().renamePage("p-beta", "Bravo")).toBe(true);
    });
    expect(registrations()).toHaveLength(before + 1);
    expect(registrations().at(-1)).toEqual([
      { pageId: "p-alpha", label: "Alpha", order: 0 },
      { pageId: "p-beta", label: "Bravo", order: 1 },
      { pageId: "p-popped", label: "Popped", order: 2 },
    ]);
  });

  it("once more after a reorder, in the new order", async () => {
    const hook = await mount();
    const before = registrations().length;
    await act(async () => {
      hook().reorderPage("p-popped", "p-alpha");
    });
    expect(registrations()).toHaveLength(before + 1);
    expect(registrations().at(-1)).toEqual([
      { pageId: "p-popped", label: "Popped", order: 0 },
      { pageId: "p-alpha", label: "Alpha", order: 1 },
      { pageId: "p-beta", label: "Beta", order: 2 },
    ]);
  });

  it("with a page reconcile synthesized for a backend page id", async () => {
    backend.terminalPageIds = ["p-minted"];
    await mount();
    expect(registrations().at(-1)).toEqual([
      { pageId: "p-alpha", label: "Alpha", order: 0 },
      { pageId: "p-beta", label: "Beta", order: 1 },
      { pageId: "p-popped", label: "Popped", order: 2 },
      { pageId: "p-minted", label: "Page 1", order: 3 },
    ]);
  });
});

describe("a page-pinned pop-out", () => {
  it("registers nothing, so its one-page view cannot replace the main window's list", async () => {
    window.history.replaceState(null, "", "/?page=p-beta");
    const hook = await mount();
    expect(hook().isPinned).toBe(true);
    await act(async () => {
      hook().renamePage("p-beta", "Bravo");
    });
    expect(registrations()).toEqual([]);
  });
});
