// @vitest-environment jsdom
/**
 * The REAL tab strip, rendered and driven in jsdom (plan
 * `2026-11-23-runner-terminal-page-popout-ui-bridge-and-resync-lag`, Phase 1).
 *
 * `TerminalPageTabBar.test.tsx` checks the markup; this file checks behaviour
 * the markup cannot show: which handler a click on the pop-out button reaches
 * now that it is a sibling of the tab button (no `stopPropagation` left to
 * keep it off `onSelectPage`), that a drop still reorders when it lands on the
 * wrapper or on an action button (the drop target moved off the tab button),
 * and which UI Bridge registrations actually ATTACH — `useUIElement` registers
 * only when its ref receives an element, so the stub below records ref
 * attachments, the same thing the SDK's registry would see.
 */

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

// id -> label for every registration whose ref currently holds an element.
const bridge = vi.hoisted(() => ({ attached: new Map<string, string | undefined>() }));
vi.mock("@qontinui/ui-bridge", () => ({
  useUIElement: (opts: { id: string; label?: string }) => ({
    ref: (el: HTMLElement | null) => {
      if (el) bridge.attached.set(opts.id, opts.label);
      else bridge.attached.delete(opts.id);
    },
  }),
}));

import type { TerminalPageConfig } from "./useTerminalPages";
import { TerminalPageTabBar } from "./TerminalPageTabBar";

const pages: TerminalPageConfig[] = [
  { id: "a", name: "Alpha", createdAt: 0 },
  { id: "b", name: "Beta", createdAt: 0 },
  { id: "c", name: "Gamma", createdAt: 0 },
];

let container: HTMLDivElement;
let root: Root;
const handlers = {
  onSelectPage: vi.fn(),
  onAddPage: vi.fn(),
  onRemovePage: vi.fn(),
  onRenamePage: vi.fn(),
  onReorderPage: vi.fn(),
  onPopOutPage: vi.fn(),
};

beforeEach(() => {
  for (const h of Object.values(handlers)) h.mockClear();
  bridge.attached.clear();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

function render(props: Partial<Parameters<typeof TerminalPageTabBar>[0]> = {}): void {
  act(() => {
    root.render(
      createElement(TerminalPageTabBar, { pages, activePageId: "a", ...handlers, ...props }),
    );
  });
}

const byTestId = (id: string): HTMLElement => {
  const el = container.querySelector<HTMLElement>(`[data-testid="${id}"]`);
  expect(el, `no element with data-testid=${id}`).not.toBeNull();
  return el!;
};

/** jsdom has no DragEvent/DataTransfer; React reads `dataTransfer` off the native event. */
function fireDrag(target: Element, type: "dragstart" | "dragover" | "drop" | "dragend"): void {
  const data = new Map<string, string>();
  const event = new Event(type, { bubbles: true, cancelable: true });
  Object.defineProperty(event, "dataTransfer", {
    value: {
      effectAllowed: "",
      dropEffect: "",
      setData: (k: string, v: string) => data.set(k, v),
      getData: (k: string) => data.get(k) ?? "",
    },
  });
  act(() => {
    target.dispatchEvent(event);
  });
}

describe("PageTab in the real strip", () => {
  it("a click on the pop-out button pops that page out and does not select it", () => {
    render();
    act(() => byTestId("terminal-page-tab-popout-b").click());
    expect(handlers.onPopOutPage).toHaveBeenCalledTimes(1);
    expect(handlers.onPopOutPage).toHaveBeenCalledWith("b");
    expect(handlers.onSelectPage).not.toHaveBeenCalled();
  });

  it("a click on the close button closes that page and does not select it", () => {
    render();
    act(() => byTestId("terminal-page-tab-close-c").click());
    expect(handlers.onRemovePage).toHaveBeenCalledWith("c");
    expect(handlers.onSelectPage).not.toHaveBeenCalled();
  });

  it("a click on the tab still selects it", () => {
    render();
    act(() => byTestId("terminal-page-tab-b").click());
    expect(handlers.onSelectPage).toHaveBeenCalledWith("b");
    expect(handlers.onPopOutPage).not.toHaveBeenCalled();
  });

  it("a drop on the target's wrapper reorders the dragged page before it", () => {
    render();
    fireDrag(byTestId("terminal-page-tab-c"), "dragstart");
    const wrapper = byTestId("terminal-page-tab-a").parentElement!;
    expect(wrapper.getAttribute("role")).toBe("presentation");
    fireDrag(wrapper, "dragover");
    fireDrag(wrapper, "drop");
    expect(handlers.onReorderPage).toHaveBeenCalledTimes(1);
    expect(handlers.onReorderPage).toHaveBeenCalledWith("c", "a");
  });

  it("a drop on the target's action button still reorders", () => {
    render();
    fireDrag(byTestId("terminal-page-tab-a"), "dragstart");
    fireDrag(byTestId("terminal-page-tab-close-b"), "dragover");
    fireDrag(byTestId("terminal-page-tab-close-b"), "drop");
    expect(handlers.onReorderPage).toHaveBeenCalledWith("a", "b");
    expect(handlers.onRemovePage).not.toHaveBeenCalled();
  });

  it("attaches each rendered control's registration under its page-id-keyed id", () => {
    render();
    expect([...bridge.attached.keys()].sort()).toEqual(
      pages
        .flatMap((p) => [
          `terminal-page-tab-${p.id}`,
          `terminal-page-tab-popout-${p.id}`,
          `terminal-page-tab-close-${p.id}`,
        ])
        .sort(),
    );
    expect(bridge.attached.get("terminal-page-tab-popout-a")).toBe("Pop out Alpha");
  });

  it("attaches no pop-out registration without onPopOutPage, and no close for the only page", () => {
    render({ pages: [pages[0]], onPopOutPage: undefined });
    expect([...bridge.attached.keys()]).toEqual(["terminal-page-tab-a"]);
  });

  it("double-click opens a focused rename input registered as terminal-page-rename-input", () => {
    render();
    act(() => {
      byTestId("terminal-page-tab-b").dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
    });
    const input = byTestId("terminal-page-rename-input") as HTMLInputElement;
    expect(document.activeElement).toBe(input);
    expect(input.value).toBe("Beta");
    expect(bridge.attached.get("terminal-page-rename-input")).toBe("Page name");
    // The edited tab's controls are not rendered, so their registrations detach.
    expect(bridge.attached.has("terminal-page-tab-b")).toBe(false);
  });

  it("the pinned strip attaches only its title registration, labelled with the page name", () => {
    render({ pages: [pages[1]], activePageId: "b", isPinned: true });
    expect([...bridge.attached.entries()]).toEqual([["terminal-page-pinned-title", "Beta"]]);
  });
});
