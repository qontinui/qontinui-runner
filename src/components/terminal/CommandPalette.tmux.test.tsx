// @vitest-environment jsdom
/**
 * The command palette's per-zone label and pin entries in tmux work mode (plan
 * `2026-10-11-runner-terminal-tmux-work-mode`). The one viewport is zone 0,
 * but zone labels and pins are keyed by GRID zone: the entries must show and
 * edit the shown session's own grid zone, and offer nothing for a session that
 * has none — never write into grid zone 0, which belongs to another session.
 */

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@/lib/instance-storage", () => ({
  instanceStorage: { getJSON: (_key: string, fallback: unknown) => fallback, setJSON: () => {} },
}));
vi.mock("./useLiveClaudeSessions", () => ({
  useLiveClaudeSessions: () => ({
    read: "ok",
    bySessionId: new Map(),
    names: new Map(),
    pids: new Set(),
  }),
}));
// A stable snapshot: `useSyncExternalStore` re-reads it every render.
const noActions = vi.hoisted(() => [] as unknown[]);
vi.mock("./commands", () => ({
  getAll: () => noActions,
  getRegistryPaletteActions: () => [],
  scorePaletteLabel: () => 0,
  subscribe: () => () => {},
}));

import { CommandPalette } from "./CommandPalette";
import type { TerminalTab } from "./useTerminalManager";

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  // jsdom lays nothing out; the palette scrolls its selected row into view.
  Element.prototype.scrollIntoView = () => {};
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

const noop = () => {};
const shown: TerminalTab = { id: "b", title: "session b", pid: 1, isAlive: true, exitCode: null };

function renderPalette(cosmeticZoneFor: (z: number) => number | null) {
  const onTogglePin = vi.fn();
  act(() => {
    root.render(
      createElement(CommandPalette, {
        onClose: noop,
        tabs: [shown],
        assignments: { 0: "b" },
        sessionStates: {},
        focusedZone: 0,
        onFocusZone: noop,
        onApproveTab: noop,
        onRejectTab: noop,
        onRestartZone: noop,
        onTogglePin,
        pinnedZones: new Set([2]),
        onApproveAll: noop,
        onSortZones: noop,
        onExport: noop,
        onToggleFocusMode: noop,
        focusMode: false,
        onToggleAutoFocus: noop,
        autoFocus: false,
        onToggleSound: noop,
        soundEnabled: false,
        zoneLabels: { 0: "zero", 2: "two" },
        onSetZoneLabel: noop,
        zoneCount: 1,
        cosmeticZoneFor,
      }),
    );
  });
  return { onTogglePin };
}

function row(id: string): HTMLElement | null {
  return container.querySelector(`[data-page-element='command-palette-option-${id}']`);
}

describe("CommandPalette in tmux mode", () => {
  it("shows and pins the shown session's own grid zone", () => {
    const { onTogglePin } = renderPalette((z) => (z === 0 ? 2 : null));
    expect(row("label-0")?.textContent).toContain("(two)");
    expect(row("pin-0")?.textContent).toContain("Unpin");
    act(() => row("pin-0")!.click());
    expect(onTogglePin.mock.calls).toEqual([[2]]);
  });

  it("offers no label or pin entry for a session with no grid zone", () => {
    renderPalette(() => null);
    expect(row("label-0")).toBeNull();
    expect(row("pin-0")).toBeNull();
    expect(row("focus-0")).not.toBeNull();
  });
});
