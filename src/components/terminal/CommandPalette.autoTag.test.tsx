// @vitest-environment jsdom
/**
 * The palette's "Auto-tag zones by working directory" (review W2), run for
 * real: it tags each occupied zone with its tab's WORKING DIRECTORY. Before
 * the fix it parsed `tab.title`, which since D3 of plan
 * `2026-10-27-click-resume-forks-a-live-conversation-and-pane-titles-hide-its-identity`
 * holds the session's name, not the shell's cwd.
 */

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@/lib/instance-storage", () => ({
  instanceStorage: { getJSON: (_key: string, fallback: unknown) => fallback, setJSON: () => {} },
}));
vi.mock("./useLiveClaudeSessionNames", () => ({ useLiveClaudeSessionNames: () => new Map() }));
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

function renderPalette(tabs: TerminalTab[], zoneLabels: Record<number, string> = {}) {
  const onSetZoneLabel = vi.fn();
  act(() => {
    root.render(
      createElement(CommandPalette, {
        onClose: noop,
        tabs,
        assignments: Object.fromEntries(tabs.map((t, z) => [z, t.id])),
        sessionStates: {},
        focusedZone: 0,
        onFocusZone: noop,
        onApproveTab: noop,
        onRejectTab: noop,
        onRestartZone: noop,
        onTogglePin: noop,
        pinnedZones: new Set<number>(),
        onApproveAll: noop,
        onSortZones: noop,
        onExport: noop,
        onToggleFocusMode: noop,
        focusMode: false,
        onToggleAutoFocus: noop,
        autoFocus: false,
        onToggleSound: noop,
        soundEnabled: false,
        zoneLabels,
        onSetZoneLabel,
        zoneCount: tabs.length,
        cosmeticZoneFor: (z: number) => z,
      }),
    );
  });
  return { onSetZoneLabel };
}

const autoTagRow = () =>
  container.querySelector<HTMLElement>("[data-page-element='command-palette-option-group-by-dir']");

describe("CommandPalette — Auto-tag zones by working directory", () => {
  it("tags each zone with its tab's working directory, not its (session-name) title", () => {
    const tabs: TerminalTab[] = [
      {
        id: "a",
        title: "maxim · 9b40e22d",
        activityTitle: "Fix Phase 5b round 11",
        workingDir: "D:/qontinui-root/qontinui-runner",
        pid: 1,
        isAlive: true,
        exitCode: null,
      },
      {
        id: "b",
        title: "PR #12: fix the gate",
        workingDir: "C:\\work\\qontinui-web",
        pid: 2,
        isAlive: true,
        exitCode: null,
      },
    ];
    const { onSetZoneLabel } = renderPalette(tabs, { 1: "web" });
    expect(autoTagRow(), "the action is offered").not.toBeNull();
    act(() => autoTagRow()!.click());
    expect(onSetZoneLabel.mock.calls).toEqual([
      [0, "qontinui-runner"],
      [1, "web, qontinui-web"],
    ]);
  });
});
