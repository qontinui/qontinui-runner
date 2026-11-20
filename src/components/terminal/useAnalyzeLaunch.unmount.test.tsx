// @vitest-environment jsdom
/**
 * The late launch re-check must stop when the Terminal page unmounts: the
 * hook's `tabs` ref freezes then, so nothing else would end the 5-minute loop.
 */
import { act, createElement, useEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const watch = vi.hoisted(() => ({ isActive: null as null | (() => boolean) }));

vi.mock("./launchVerification", async (orig) => {
  const real = await orig<typeof import("./launchVerification")>();
  return {
    ...real,
    watchFreshLaunch: vi.fn((_tabId: string, deps: { isActive: () => boolean }) => {
      watch.isActive = deps.isActive;
      return new Promise(() => {});
    }),
  };
});
vi.mock("@qontinui/ui-bridge", () => ({ useUIComponent: () => undefined }));

import { useAnalyzeLaunch } from "./useAnalyzeLaunch";
import type { TerminalTab } from "./useTerminalManager";

type Api = ReturnType<typeof useAnalyzeLaunch>;

function args() {
  return {
    tabs: [{ id: "t1" } as unknown as TerminalTab],
    updateTab: vi.fn(),
    createPane: vi.fn(),
    write: vi.fn(),
    accounts: [],
    launchAliases: undefined,
    notify: vi.fn(),
    onLaunched: vi.fn(),
    setActiveId: vi.fn(),
    zoneAssignments: {},
    setFocusedZone: vi.fn(),
  };
}

let container: HTMLDivElement;
let root: Root;
const holder: { api: Api | null } = { api: null };
const TAB_ARGS = args();

function Harness() {
  const api = useAnalyzeLaunch(TAB_ARGS);
  useEffect(() => {
    holder.api = api;
  });
  return null;
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  watch.isActive = null;
  holder.api = null;
  container = document.createElement("div");
  root = createRoot(container);
  act(() => root.render(createElement(Harness)));
});

afterEach(() => {
  vi.clearAllMocks();
});

describe("useAnalyzeLaunch verifyLaunch", () => {
  it("keeps a watch active while mounted and ends it on unmount", () => {
    act(() => holder.api!.verifyLaunch("t1"));
    expect(watch.isActive).not.toBeNull();
    expect(watch.isActive!()).toBe(true);
    act(() => root.unmount());
    expect(watch.isActive!()).toBe(false);
  });

  it("ends an older watch when a newer launch starts in the same tab", () => {
    act(() => holder.api!.verifyLaunch("t1"));
    const first = watch.isActive!;
    act(() => holder.api!.verifyLaunch("t1"));
    expect(first()).toBe(false);
    expect(watch.isActive!()).toBe(true);
    act(() => root.unmount());
  });
});
