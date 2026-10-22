/**
 * Restart-in-zone driven against a REAL `useZoneLayout`, in both work modes:
 * the replacement must take the finished pane's place — its zone, and in tmux
 * mode the grid zone (with its label) the session returns to — and the old
 * pane is retired only once the replacement exists.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("react", async () => {
  const { hooksHarness } = await import("@/lib/__test-helpers__/hooks-harness");
  return hooksHarness.reactMock;
});

const storage = vi.hoisted(() => ({ value: null as unknown }));
vi.mock("@/lib/instance-storage", () => ({
  instanceStorage: {
    getJSON: () => storage.value,
    setJSON: () => {},
  },
}));

import { hooksHarness as harness } from "@/lib/__test-helpers__/hooks-harness";
import { useZoneLayout } from "./useZoneLayout";
import { restartInZone, type RestartInZoneDeps } from "./restartInZone";

/** A page: the live tab list, the real zone layout over it, and its labels. */
function mountPage(persisted: unknown, initialTabs: string[]) {
  harness.reset();
  storage.value = persisted;
  let tabIds = initialTabs;
  function ZoneLayoutHost() {
    harness.beginRender();
    return useZoneLayout(tabIds, "restart-test", "main");
  }
  const render = () => harness.renderSettled(ZoneLayoutHost);
  const labels: Record<number, string> = {};
  const closed: string[] = [];
  const closeOptions: unknown[] = [];
  const setZoneLabel = vi.fn((zone: number, label: string) => {
    labels[zone] = label;
  });
  /** Restart `zoneIdx`, with `spawned` as the id the replacement gets (or a failed spawn). */
  async function restart(zoneIdx: number, spawned: string | null) {
    const layout = render();
    const deps: RestartInZoneDeps = {
      zoneLayout: layout,
      tabs: tabIds.map((id) => ({ id, title: id, workingDir: `/w/${id}` })),
      sessionStates: Object.fromEntries(tabIds.map((id) => [id, "completed"])),
      zoneLabels: { ...labels },
      setZoneLabel,
      createTerminal: async () => {
        if (spawned !== null) tabIds = [...tabIds, spawned];
        return spawned;
      },
      closeTerminal: (tabId, options) => {
        closed.push(tabId);
        closeOptions.push(options);
        tabIds = tabIds.filter((id) => id !== tabId);
      },
    };
    const outcome = await restartInZone(deps, zoneIdx);
    return { outcome, after: render() };
  }
  return { render, labels, closed, closeOptions, setZoneLabel, restart };
}

const grid = (layoutId: string, assignments: Record<number, string>, focusedZone = 0) => ({
  layoutId,
  assignments,
  focusedZone,
});

beforeEach(() => harness.reset());

describe("restart in zone", () => {
  it("retires the replaced pane immediately, with no Undo window", async () => {
    const page = mountPage(grid("quad", { 0: "a", 1: "b" }, 0), ["a", "b"]);

    const { outcome } = await page.restart(0, "n");

    expect(outcome).toStrictEqual({ restarted: true, tabId: "n", retiredTabId: "a" });
    // A restart REPLACES the pane; the retirement is not an operator close, so
    // it must not hold the old PTY open for the 10 s Undo window.
    expect(page.closeOptions).toStrictEqual([{ immediate: true }]);
  });

  it("tmux: keeps the session's grid zone and label, and comes back from tmux in it", async () => {
    const page = mountPage(grid("quad", { 0: "a", 1: "b", 2: "c" }, 2), ["a", "b", "c"]);
    page.labels[2] = "build";
    page.render().enterTmuxMode();
    expect(page.render().shownTabId).toBe("c");

    const { outcome, after } = await page.restart(0, "n");

    expect(outcome).toStrictEqual({ restarted: true, tabId: "n", retiredTabId: "c" });
    expect(page.closed).toStrictEqual(["c"]);
    expect(after.shownTabId).toBe("n");
    // The viewport's cosmetics are still the session's grid zone, label intact.
    expect(after.cosmeticZoneFor(0)).toBe(2);
    expect(page.setZoneLabel).toHaveBeenCalledWith(2, "build");
    expect(after.gridView.assignments).toStrictEqual({ 0: "a", 1: "b", 2: "n" });

    after.exitTmuxMode();
    const grid2 = page.render();
    expect(grid2.assignments).toStrictEqual({ 0: "a", 1: "b", 2: "n" });
    expect(grid2.focusedZone).toBe(2);
  });

  it("grid: the replacement takes the zone, keeps its label, and is focused", async () => {
    const page = mountPage(grid("quad", { 0: "a", 1: "b" }, 0), ["a", "b"]);
    page.labels[1] = "tests";

    const { outcome, after } = await page.restart(1, "n");

    expect(outcome).toStrictEqual({ restarted: true, tabId: "n", retiredTabId: "b" });
    expect(after.assignments).toStrictEqual({ 0: "a", 1: "n" });
    expect(after.focusedZone).toBe(1);
    expect(page.setZoneLabel).toHaveBeenCalledWith(1, "tests");
  });

  it("a failed spawn retires nothing and leaves the zone as it was", async () => {
    const page = mountPage(grid("quad", { 0: "a", 1: "b" }, 0), ["a", "b"]);

    const { outcome, after } = await page.restart(1, null);

    expect(outcome).toStrictEqual({ restarted: false, reason: "spawn-failed" });
    expect(page.closed).toStrictEqual([]);
    expect(after.assignments).toStrictEqual({ 0: "a", 1: "b" });
  });

  it("an empty zone is not restartable", async () => {
    const page = mountPage(grid("quad", { 0: "a" }, 0), ["a"]);

    const { outcome } = await page.restart(3, "n");

    expect(outcome).toStrictEqual({ restarted: false, reason: "not-restartable", state: "idle" });
  });
});
