// @vitest-environment jsdom
/**
 * The narrow-zone overflow menu, driven in a DOM (plan
 * `2026-10-22-runner-zone-header-total-output-seed-and-narrow-zone-chips`
 * Phase 2). jsdom evaluates no container query, so the menu button is in the
 * DOM at every width here; what is under test is its behaviour as a menu
 * button: open/close and `aria-expanded`, focus management, keyboard, outside
 * click, and the in-place export / window lists.
 */

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const h = vi.hoisted(() => ({
  toggleMaximize: vi.fn(),
  handleRestartInZone: vi.fn(),
  listWindows: vi.fn(() => Promise.resolve([{ label: "main" }, { label: "term-2" }])),
  moveTabToWindow: vi.fn(() => Promise.resolve()),
  state: { session: "completed" as string },
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => Promise.resolve(null)) }));
vi.mock("./contexts", () => ({
  useTerminalSession: () => ({
    zoneLayout: {
      assignments: { 0: "a" },
      maximizedZone: null,
      workMode: "grid",
      toggleMaximize: h.toggleMaximize,
    },
    closeTerminal: () => {},
    sessionStates: { a: h.state.session },
    tabs: [],
  }),
  useTransitionEffects: () => ({ handleRestartInZone: h.handleRestartInZone }),
  useZoneMetadata: () => ({ labelsAndTags: { zoneLabels: {}, setZoneLabel: () => {} } }),
}));
vi.mock("./contexts/WindowAssignmentsContext", () => ({
  useWindowAssignments: () => ({ ownerOf: () => "main" }),
}));
vi.mock("./useTerminalWindowActions", () => ({
  useTerminalWindowActions: () => ({
    popOutTab: () => Promise.resolve(),
    moveTabToWindow: h.moveTabToWindow,
    listWindows: h.listWindows,
  }),
}));

import { ZoneHoverActions, type ZoneHeaderMenuActions } from "./ZoneHoverActions";

let container: HTMLDivElement;
let root: Root;
const onExportZone = vi.fn();

function mount(headerActions?: ZoneHeaderMenuActions) {
  act(() => {
    root.render(
      createElement(ZoneHoverActions, { zoneIdx: 0, labelZone: 0, onExportZone, headerActions }),
    );
  });
}

const button = () =>
  container.querySelector<HTMLButtonElement>('[aria-haspopup="menu"][aria-label="Zone actions"]')!;
const menu = () => container.querySelector<HTMLElement>('[role="menu"]')!;
const items = () => Array.from(menu().querySelectorAll<HTMLElement>('[role="menuitem"]'));
const item = (text: string) => items().find((el) => el.textContent?.trim() === text)!;
const isOpen = () => button().getAttribute("aria-expanded") === "true" && !menu().hidden;
const key = (el: Element, k: string) =>
  act(() => {
    el.dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true }));
  });
const click = (el: HTMLElement) => act(() => el.click());

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  h.state.session = "completed";
  vi.clearAllMocks();
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("ZoneHoverActions overflow menu", () => {
  it("is one menu button, closed until clicked, and toggles open and shut", () => {
    mount();
    expect(container.querySelectorAll('[aria-haspopup="menu"]')).toHaveLength(1);
    expect(button().getAttribute("aria-controls")).toBe(menu().id);
    expect(isOpen()).toBe(false);
    click(button());
    expect(isOpen()).toBe(true);
    click(button());
    expect(isOpen()).toBe(false);
  });

  it("holds the five collapsible actions, named by their visible text", () => {
    mount();
    click(button());
    expect(items().map((el) => el.textContent?.trim())).toEqual([
      "Maximize zone",
      "Restart session",
      "Edit zone label",
      "Export zone output",
      "Send terminal to a window",
    ]);
    for (const el of items()) expect(el.hasAttribute("aria-label")).toBe(false);
  });

  it("moves focus to the first item on open", () => {
    mount();
    click(button());
    expect(document.activeElement).toBe(items()[0]);
  });

  it("Escape closes it and returns focus to the button", () => {
    mount();
    click(button());
    key(document.activeElement!, "Escape");
    expect(isOpen()).toBe(false);
    expect(document.activeElement).toBe(button());
  });

  it("ArrowDown / ArrowUp move between items and wrap", () => {
    mount();
    click(button());
    const all = items();
    key(document.activeElement!, "ArrowUp");
    expect(document.activeElement).toBe(all[all.length - 1]);
    key(document.activeElement!, "ArrowDown");
    expect(document.activeElement).toBe(all[0]);
    key(document.activeElement!, "ArrowDown");
    expect(document.activeElement).toBe(all[1]);
  });

  it("ArrowDown / ArrowUp on the closed button open it onto the first / last item", () => {
    mount();
    button().focus();
    key(button(), "ArrowDown");
    expect(isOpen()).toBe(true);
    expect(document.activeElement).toBe(items()[0]);
    key(document.activeElement!, "Escape");
    key(button(), "ArrowUp");
    expect(isOpen()).toBe(true);
    expect(document.activeElement).toBe(items()[items().length - 1]);
  });

  it("Tab closes it", () => {
    mount();
    click(button());
    key(document.activeElement!, "Tab");
    expect(isOpen()).toBe(false);
  });

  it("an outside mousedown closes it; one inside does not", () => {
    mount();
    click(button());
    act(() => {
      items()[1].dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    });
    expect(isOpen()).toBe(true);
    act(() => {
      document.body.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    });
    expect(isOpen()).toBe(false);
  });

  it("the pointer leaving the cell closes it, so nothing unseen keeps focus", () => {
    container.classList.add("group");
    mount();
    click(button());
    expect(isOpen()).toBe(true);
    act(() => {
      container.dispatchEvent(new MouseEvent("mouseleave"));
    });
    expect(isOpen()).toBe(false);
    container.classList.remove("group");
  });

  it("focus moving to another element closes it", () => {
    mount();
    click(button());
    const elsewhere = document.createElement("button");
    document.body.appendChild(elsewhere);
    act(() => elsewhere.focus());
    expect(isOpen()).toBe(false);
    elsewhere.remove();
  });

  it("an action runs and closes the menu", () => {
    mount();
    click(button());
    click(item("Maximize zone"));
    expect(h.toggleMaximize).toHaveBeenCalledWith(0);
    expect(isOpen()).toBe(false);
  });

  it("a keyboard action returns focus to the button rather than dropping it to the page", () => {
    mount();
    click(button());
    item("Restart session").focus();
    // Enter on a button clicks it with `detail` 0, as `el.click()` does.
    click(item("Restart session"));
    expect(h.handleRestartInZone).toHaveBeenCalledWith(0);
    expect(document.activeElement).toBe(button());
  });

  it("a mouse action leaves focus off the button, so later typing cannot run an item", () => {
    mount();
    click(button());
    const restart = item("Restart session");
    restart.focus();
    act(() => {
      restart.dispatchEvent(new MouseEvent("click", { bubbles: true, detail: 1 }));
    });
    expect(h.handleRestartInZone).toHaveBeenCalledWith(0);
    expect(isOpen()).toBe(false);
    expect(document.activeElement).not.toBe(button());
  });

  it("the menu and the label popover fit the cell rather than losing their left end", () => {
    mount();
    expect(menu().className).toContain("max-w-[calc(100cqw-58px)]");
    click(button());
    click(item("Edit zone label"));
    const popover = container.querySelector("input")!.parentElement!;
    expect(popover.className).toContain("max-w-[calc(100cqw-8px)]");
  });

  it("a held Enter that opened the label popover does not submit it", () => {
    mount();
    click(button());
    click(item("Edit zone label"));
    const input = () => container.querySelector("input");
    act(() => {
      input()!.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Enter", repeat: true, bubbles: true }),
      );
    });
    expect(input()).not.toBeNull();
    act(() => {
      input()!.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    });
    expect(input()).toBeNull();
  });

  it("a held Enter runs nothing, on the button or on the menu it opened", () => {
    mount();
    const held = () =>
      new KeyboardEvent("keydown", {
        key: "Enter",
        repeat: true,
        bubbles: true,
        cancelable: true,
      });
    const onButton = held();
    act(() => {
      button().dispatchEvent(onButton);
    });
    expect(onButton.defaultPrevented).toBe(true);
    click(button());
    const onItem = held();
    act(() => {
      items()[0]!.dispatchEvent(onItem);
    });
    expect(onItem.defaultPrevented).toBe(true);
    click(button());
    const first = new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true });
    act(() => {
      button().dispatchEvent(first);
    });
    expect(first.defaultPrevented).toBe(false);
  });

  it("carries the row toggles it is handed, each running its handler", () => {
    const onTogglePin = vi.fn();
    const onToggleFilter = vi.fn();
    const onTogglePrompts = vi.fn();
    const onToggleReview = vi.fn();
    const onScrollToBottom = vi.fn();
    mount({
      isPinned: true,
      onTogglePin,
      onToggleFilter,
      onTogglePrompts,
      onToggleReview,
      onScrollToBottom,
    });
    click(button());
    expect(menu().querySelector('[role="group"][aria-label="Zone header"]')).not.toBeNull();
    for (const [text, handler] of [
      ["Unpin zone", onTogglePin],
      ["Filter output", onToggleFilter],
      ["Show my prompts", onTogglePrompts],
      ["Review session changes", onToggleReview],
      ["Scroll to bottom", onScrollToBottom],
    ] as const) {
      if (!isOpen()) click(button());
      click(item(text));
      expect(handler, text).toHaveBeenCalledTimes(1);
      expect(isOpen(), text).toBe(false);
    }
  });

  it("names each toggle by its state", () => {
    const f = () => {};
    mount({
      isPinned: false,
      onTogglePin: f,
      filterActive: true,
      onToggleFilter: f,
      promptsVisible: true,
      onTogglePrompts: f,
      reviewOpen: true,
      onToggleReview: f,
    });
    click(button());
    expect(item("Pin zone")).toBeTruthy();
    expect(item("Filter output (active)")).toBeTruthy();
    expect(item("Hide my prompts")).toBeTruthy();
    expect(item("Hide session review")).toBeTruthy();
  });

  it("leaves out a row toggle with no handler, and the group with none", () => {
    mount({ onScrollToBottom: () => {} });
    click(button());
    expect(item("Scroll to bottom")).toBeTruthy();
    expect(item("Pin zone")).toBeUndefined();
    expect(item("Filter output")).toBeUndefined();
    expect(item("Show my prompts")).toBeUndefined();
    act(() => root.unmount());
    root = createRoot(container);
    mount();
    click(button());
    expect(menu().querySelector('[aria-label="Zone header"]')).toBeNull();
  });

  it("Edit zone label moves focus on into the label input", async () => {
    mount();
    click(button());
    await act(async () => {
      item("Edit zone label").click();
      await Promise.resolve();
    });
    expect(isOpen()).toBe(false);
    expect(document.activeElement?.tagName).toBe("INPUT");
  });

  it("a disabled item stays focusable, does nothing, and leaves the menu open", () => {
    h.state.session = "working";
    mount();
    click(button());
    const restart = item("Restart session");
    expect(restart.getAttribute("aria-disabled")).toBe("true");
    expect(restart.hasAttribute("disabled")).toBe(false);
    click(restart);
    expect(h.handleRestartInZone).not.toHaveBeenCalled();
    expect(isOpen()).toBe(true);
  });

  it("Export expands its formats in place, in a group, and a format exports and closes", () => {
    mount();
    click(button());
    const exp = item("Export zone output");
    // It expands an inline group, not a submenu: no aria-haspopup.
    expect(exp.hasAttribute("aria-haspopup")).toBe(false);
    expect(exp.getAttribute("aria-expanded")).toBe("false");
    click(exp);
    expect(exp.getAttribute("aria-expanded")).toBe("true");
    const group = menu().querySelector('[role="group"][aria-label="Export formats"]');
    expect(group?.textContent).toBe("textmarkdownjson");
    // The full cluster's own export dropdown is not rendered a second time.
    expect(container.querySelectorAll('[role="group"], .w-24')).toHaveLength(1);
    click(item("markdown"));
    expect(onExportZone).toHaveBeenCalledWith(0, "markdown");
    expect(isOpen()).toBe(false);
  });

  it("Send to a window expands the window list in place and moves the terminal", async () => {
    mount();
    click(button());
    click(item("Send terminal to a window"));
    await act(async () => {
      await Promise.resolve();
    });
    expect(h.listWindows).toHaveBeenCalledTimes(1);
    const group = menu().querySelector('[role="group"][aria-label="Windows"]');
    // "main" owns this tab, so it is not offered.
    expect(group?.textContent).toBe("New windowterm-2");
    click(item("term-2"));
    expect(h.moveTabToWindow).toHaveBeenCalledWith("a", "term-2");
    expect(isOpen()).toBe(false);
  });

  it("opening it closes a label popover opened from the full cluster", () => {
    mount();
    const fullLabel = container.querySelector<HTMLButtonElement>('[aria-label="Edit zone label"]')!;
    click(fullLabel);
    expect(container.querySelector("input[placeholder^='zone label']")).not.toBeNull();
    click(button());
    expect(isOpen()).toBe(true);
    expect(container.querySelector("input[placeholder^='zone label']")).toBeNull();
  });

  it("opening it closes a full-cluster dropdown, and opening one closes it", () => {
    mount();
    const fullExport = container.querySelector<HTMLButtonElement>(
      '[aria-label="Export zone output"]',
    )!;
    click(fullExport);
    expect(fullExport.getAttribute("aria-expanded")).toBe("true");
    click(button());
    expect(fullExport.getAttribute("aria-expanded")).toBe("false");
    expect(isOpen()).toBe(true);
    click(fullExport);
    expect(isOpen()).toBe(false);
  });
});
