/**
 * `+ new terminal` quick-launch layout decision — plan
 * 2026-08-23-single-source-derived-facts item 7.
 *
 * `useZoneActions.createAndAssignTerminal` carried an inline copy of the
 * `pickLayout` ladder with no `>= 10 → flow-grid` rung. From the 11th tab on it
 * called `setLayoutId("full-grid")` while the layout was already `flow-grid`,
 * SHRINKING it (11 zones → 9); `applyLayoutAssignments` then compacted every
 * tab at zone ≥ 9 into the lowest holes, and the auto-grow effect regrew the
 * layout but not the arrangement.
 *
 * The hook is driven for real: `renderToStaticMarkup` runs the hook body once
 * (vitest is `environment: "node"`, no DOM) and the captured
 * `createAndAssignTerminal` closure is then invoked outside render.
 */

import { describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

vi.mock("@tauri-apps/plugin-dialog", () => ({ save: vi.fn() }));
vi.mock("@tauri-apps/plugin-fs", () => ({ writeTextFile: vi.fn() }));
vi.mock("./terminalHotStore", () => ({
  getTerminalHotStore: () => ({ getLastOutputLines: () => [] }),
}));

import { useZoneActions } from "./useZoneActions";
import {
  applyLayoutAssignments,
  computeQuickLaunchLayoutId,
  reconcileAssignments,
  resolveLayout,
  type ZoneAssignments,
} from "./useZoneLayout";
import type { TerminalTab } from "./useTerminalManager";

function tabIds(n: number): string[] {
  return Array.from({ length: n }, (_, i) => `t${i}`);
}

function denseAssignments(ids: string[]): ZoneAssignments {
  const out: ZoneAssignments = {};
  ids.forEach((id, i) => (out[i] = id));
  return out;
}

/**
 * Mount the hook against `existing` tabs in `layoutId`, spawn one terminal,
 * and return every `setLayoutId` call it made.
 */
async function spawnOne(
  existing: number,
  layoutId: string,
  assignments?: ZoneAssignments,
): Promise<string[]> {
  const ids = tabIds(existing);
  const layout = resolveLayout(layoutId, existing);
  const setLayoutId = vi.fn();
  let actions: ReturnType<typeof useZoneActions> | null = null;
  function Harness() {
    actions = useZoneActions({
      pageId: "page-1",
      tabs: ids.map((id) => ({ id }) as unknown as TerminalTab),
      dispatch: () => {},
      zoneLayout: {
        layoutId,
        assignments: assignments ?? denseAssignments(ids.slice(0, layout.zones.length)),
        layout,
        setFocusedZone: () => {},
        assignTabToZone: () => {},
        setLayoutId,
        isMultiZone: layout.zones.length > 1,
        toggleMaximize: () => {},
      },
      stateTracking: { sessionStates: {} },
      labelsAndTags: { zoneLabels: {}, setZoneLabel: () => {} },
      transitionEffects: { setUnseenNeedsInput: () => {} },
      createTerminal: async () => "t-new",
      createPlanTab: () => null,
      incrementMetric: () => {},
      setNotification: () => {},
    });
    return null;
  }
  renderToStaticMarkup(<Harness />);
  expect(actions).not.toBeNull();
  await actions!.createAndAssignTerminal();
  return setLayoutId.mock.calls.map((c) => c[0] as string);
}

describe("createAndAssignTerminal — layout choice rides the canonical pickLayout", () => {
  it("at 9 existing tabs in a full full-grid, grows into flow-grid (not full-grid)", async () => {
    expect(await spawnOne(9, "full-grid")).toEqual(["flow-grid"]);
  });

  it("at 10 existing tabs (already flow-grid) does NOT call setLayoutId('full-grid')", async () => {
    const calls = await spawnOne(10, "flow-grid");
    expect(calls).not.toContain("full-grid");
    expect(calls).toEqual([]);
  });

  it("at 11 existing tabs in flow-grid, the 12th spawn leaves the layout alone", async () => {
    expect(await spawnOne(11, "flow-grid")).toEqual([]);
  });

  it("negative control: 4 tabs in a full quad still grows to six-pack on the 5th", async () => {
    // A fix that froze the layout would pass the three cases above.
    expect(await spawnOne(4, "quad")).toEqual(["six-pack"]);
  });

  it("negative control: an empty zone absorbs the spawn with no layout change", async () => {
    // quad with 2 tabs in zones 0/1 — zones 2/3 are free.
    expect(await spawnOne(2, "quad", { 0: "t0", 1: "t1" })).toEqual([]);
  });
});

describe("the assignment-scramble regression", () => {
  // 11 tabs in flow-grid with a distinctive arrangement: `t-known` parked at
  // zone 10, zone 3 deliberately empty (a hole compaction would fill).
  const ids = [...tabIds(10), "t-known"];
  const arranged: ZoneAssignments = {};
  [0, 1, 2, 4, 5, 6, 7, 8, 9].forEach((z, i) => (arranged[z] = ids[i]));
  arranged[11] = ids[9];
  arranged[10] = "t-known";

  it("spawning the 12th tab keeps the tab at zone 10 at zone 10", () => {
    const withNew = [...ids, "t-new"];
    // The quick-launch decision no longer shrinks the layout...
    expect(computeQuickLaunchLayoutId("flow-grid", 11, true, 12)).toBeNull();
    // ...so the only assignment pass is the hook's reconcile at the grown
    // zone count, which preserves every in-range assignment.
    const next = reconcileAssignments(arranged, withNew, 12);
    expect(next[10]).toBe("t-known");
    // The new tab lands in the hole, not on top of anyone.
    expect(next[3]).toBe("t-new");
  });

  it("anti-vacuity: the old shrink path WOULD have moved it", () => {
    // What `setLayoutId("full-grid")` fed `applyLayoutAssignments` before the
    // fix. Zones 9-11 do not exist in a 9-zone layout: their tabs overflow into
    // the lowest holes (here only zone 3), and whatever does not fit is left
    // unzoned — `t-known` loses zone 10 either way. That is the scramble.
    const shrunk = applyLayoutAssignments(arranged, ids, 9);
    expect(shrunk[10]).toBeUndefined();
    expect(Object.values(shrunk)).not.toContain("t-known");
  });
});

describe("computeQuickLaunchLayoutId — grow-only", () => {
  it("never returns a layout with fewer zones than the current one", () => {
    // Every zone of a 9-zone full-grid occupied by 3 stale assignments' worth
    // of tabs: pickLayout(4) is quad (4 zones) — a shrink, refused.
    expect(computeQuickLaunchLayoutId("full-grid", 9, false, 4)).toBeNull();
  });
});
