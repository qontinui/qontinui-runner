// @vitest-environment jsdom
/**
 * The hover cluster's label popover edits ONE zone's label. In tmux mode the
 * zone it edits (`labelZone`, the shown session's grid zone) changes under it
 * whenever the operator swaps sessions by keyboard; a popover left open would
 * then write the old session's draft onto the new session's label. It closes
 * instead.
 */

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const setZoneLabel = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => Promise.resolve(null)) }));
vi.mock("./contexts", () => ({
  useTerminalSession: () => ({
    zoneLayout: {
      assignments: { 0: "a" },
      maximizedZone: null,
      workMode: "tmux",
      toggleMaximize: () => {},
    },
    closeTerminal: () => {},
    sessionStates: {},
    tabs: [],
  }),
  useTransitionEffects: () => ({ handleRestartInZone: () => Promise.resolve() }),
  useZoneMetadata: () => ({
    labelsAndTags: { zoneLabels: { 1: "alpha", 2: "beta" }, setZoneLabel },
  }),
}));
vi.mock("./contexts/WindowAssignmentsContext", () => ({
  useWindowAssignments: () => ({ ownerOf: () => null }),
}));
vi.mock("./useTerminalWindowActions", () => ({
  useTerminalWindowActions: () => ({
    popOutTab: () => Promise.resolve(),
    moveTabToWindow: () => Promise.resolve(),
    listWindows: () => Promise.resolve([]),
  }),
}));

import { ZoneHoverActions } from "./ZoneHoverActions";

let container: HTMLDivElement;
let root: Root;

function renderActions(labelZone: number | null) {
  act(() => {
    root.render(createElement(ZoneHoverActions, { zoneIdx: 0, labelZone }));
  });
}

function labelInput(): HTMLInputElement | null {
  return container.querySelector("input[placeholder^='zone label']");
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  setZoneLabel.mockClear();
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("ZoneHoverActions label popover", () => {
  it("opens on the zone it was opened for, seeded with that zone's label", () => {
    renderActions(1);
    const button = container.querySelector<HTMLButtonElement>("[aria-label='Edit zone label']");
    act(() => button?.click());
    expect(labelInput()?.value).toBe("alpha");
  });

  it("closes, writing nothing, when the zone it edits changes under it", () => {
    renderActions(1);
    act(() =>
      container.querySelector<HTMLButtonElement>("[aria-label='Edit zone label']")?.click(),
    );
    expect(labelInput()).not.toBeNull();

    // A keyboard swap in tmux shows another session: its cosmetics are zone 2.
    renderActions(2);

    expect(labelInput()).toBeNull();
    expect(setZoneLabel).not.toHaveBeenCalled();
  });

  it("stays open while the zone it edits is unchanged", () => {
    renderActions(1);
    act(() =>
      container.querySelector<HTMLButtonElement>("[aria-label='Edit zone label']")?.click(),
    );
    renderActions(1);
    expect(labelInput()?.value).toBe("alpha");
  });
});
