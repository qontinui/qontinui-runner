// @vitest-environment jsdom
/**
 * Page naming through the mounted hook and tab bar (plan 2026-11-05 Phase 5):
 * the "+" button, the no-visible-page fallback and the rename refusal, which
 * all decide a name against the FULL roster — including pages detached into a
 * pop-out, which the tab bar cannot see.
 */

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  // Every backend read fails: reconcile is best-effort and adds nothing.
  invoke: vi.fn(() => Promise.reject(new Error("no backend in test"))),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));

import { instanceStorage } from "@/lib/instance-storage";
import { setPageBindings } from "@/lib/pageBindings";
import { TerminalPageTabBar } from "./TerminalPageTabBar";
import { PAGES_STORAGE_KEY, useTerminalPages, type TerminalPageConfig } from "./useTerminalPages";

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  Element.prototype.scrollIntoView = () => {};
  localStorage.clear();
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  localStorage.clear();
});

type Hook = ReturnType<typeof useTerminalPages>;

/** Mount the hook AND the real tab bar wired to it, the way `App.tsx` does. */
function mount(): { hook: () => Hook } {
  let latest: Hook | null = null;
  function Harness() {
    const h = useTerminalPages();
    latest = h;
    return createElement(TerminalPageTabBar, {
      pages: h.visiblePages,
      activePageId: h.activePageId,
      onSelectPage: h.setActivePageId,
      onAddPage: h.addPage,
      onRemovePage: (id: string) => void h.removePage(id),
      onRenamePage: h.renamePage,
    });
  }
  act(() => root.render(createElement(Harness)));
  return { hook: () => latest! };
}

function seed(pages: TerminalPageConfig[], bindings: Record<string, string> = {}) {
  instanceStorage.setJSON(PAGES_STORAGE_KEY, pages);
  setPageBindings(bindings);
}

const persisted = () => instanceStorage.getJSON<TerminalPageConfig[]>(PAGES_STORAGE_KEY, []);

describe('the tab bar\'s "+"', () => {
  it('with a detached "Page 7" yields "Page 8"', () => {
    seed(
      [
        { id: "p1", name: "Page 1", createdAt: 1 },
        { id: "p7", name: "Page 7", createdAt: 2 },
      ],
      { p7: "terminal-popout-1" },
    );
    const { hook } = mount();
    // The strip shows only Page 1; the old rule (`pages.length + 1`) said "Page 2".
    expect(hook().visiblePages.map((p) => p.name)).toEqual(["Page 1"]);

    const add = container.querySelector<HTMLButtonElement>('[aria-label="Add terminal page"]')!;
    act(() => add.click());

    const added = persisted().at(-1)!;
    expect(added.name).toBe("Page 8");
    expect(added.origin).toBe("operator");
    expect(hook().activePageId).toBe(added.id);
  });
});

describe("the no-visible-page fallback", () => {
  it('never yields "Terminal" and marks its page auto', () => {
    // Every page is detached: the main window mints one so it has a tab.
    seed([{ id: "p3", name: "Page 3", createdAt: 1 }], { p3: "terminal-popout-1" });
    mount();

    const roster = persisted();
    expect(roster).toHaveLength(2);
    const minted = roster[1];
    expect(minted.name).toBe("Page 4");
    expect(minted.name).not.toBe("Terminal");
    expect(minted.origin).toBe("auto");
  });
});

describe("addPage with a name", () => {
  it("makes the name unique against the full roster", () => {
    seed(
      [
        { id: "p1", name: "Page 1", createdAt: 1 },
        { id: "det", name: "tmux", createdAt: 2 },
      ],
      { det: "terminal-popout-1" },
    );
    const { hook } = mount();
    act(() => void hook().addPage("tmux"));
    act(() => void hook().addPage("Terminal"));
    expect(persisted().map((p) => p.name)).toEqual(["Page 1", "tmux", "tmux 2", "Page 2"]);
  });
});

describe("rename", () => {
  const roster: TerminalPageConfig[] = [
    { id: "default", name: "Terminal", createdAt: 0 },
    { id: "a", name: "tmux", createdAt: 1 },
    { id: "b", name: "Page 2", createdAt: 2 },
  ];

  it('refuses a used name or a non-default "Terminal" and persists nothing', () => {
    seed(roster);
    const { hook } = mount();
    let ok = true;
    act(() => void (ok = hook().renamePage("b", "tmux")));
    expect(ok).toBe(false);
    act(() => void (ok = hook().renamePage("b", "Terminal")));
    expect(ok).toBe(false);
    expect(persisted().map((p) => p.name)).toEqual(["Terminal", "tmux", "Page 2"]);

    act(() => void (ok = hook().renamePage("b", "agentic")));
    expect(ok).toBe(true);
    expect(persisted().map((p) => p.name)).toEqual(["Terminal", "tmux", "agentic"]);
  });

  it('shows "Name in use" inline and keeps the input open', () => {
    // A detached page holds the name: the strip cannot see it, the hook can.
    seed(
      [
        { id: "a", name: "Page 1", createdAt: 1 },
        { id: "det", name: "resumes", createdAt: 2 },
      ],
      { det: "terminal-popout-1" },
    );
    mount();
    const tab = [...container.querySelectorAll<HTMLButtonElement>('[role="tab"]')].find(
      (b) => b.textContent === "Page 1",
    )!;
    act(() => {
      tab.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
    });
    const input = container.querySelector<HTMLInputElement>('input[aria-label="Page name"]')!;
    act(() => {
      const setValue = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
      setValue.call(input, "resumes");
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    act(() => {
      input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    });

    expect(container.querySelector('[role="alert"]')?.textContent).toBe("Name in use");
    expect(container.querySelector('input[aria-label="Page name"]')).not.toBeNull();
    expect(persisted().map((p) => p.name)).toEqual(["Page 1", "resumes"]);

    // Clicking away from a refused name drops the edit and keeps the old name.
    act(() => {
      input.dispatchEvent(new FocusEvent("focusout", { bubbles: true }));
    });
    expect(container.querySelector('input[aria-label="Page name"]')).toBeNull();
    expect(container.querySelector('[role="alert"]')).toBeNull();
    expect(persisted().map((p) => p.name)).toEqual(["Page 1", "resumes"]);
  });

  it("stores the trimmed name it checked", () => {
    seed([
      { id: "a", name: "tmux", createdAt: 1 },
      { id: "b", name: "Page 2", createdAt: 2 },
    ]);
    const { hook } = mount();
    let ok = true;
    act(() => void (ok = hook().renamePage("b", "tmux ")));
    expect(ok).toBe(false);
    act(() => void (ok = hook().renamePage("b", "  agentic  ")));
    expect(ok).toBe(true);
    expect(persisted().map((p) => p.name)).toEqual(["tmux", "agentic"]);
  });
});
