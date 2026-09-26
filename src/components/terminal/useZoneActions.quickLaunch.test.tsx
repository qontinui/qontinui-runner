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
 * and return every `setLayoutId` call it made. Pass `ids` when `assignments`
 * names specific tabs, so the hook sees the same tab roster the assignments
 * describe.
 */
async function spawnOne(
  existing: number,
  layoutId: string,
  assignments?: ZoneAssignments,
  ids: string[] = tabIds(existing),
): Promise<string[]> {
  expect(ids).toHaveLength(existing);
  if (assignments) {
    // Every assigned tab must be one the hook is told exists.
    for (const id of Object.values(assignments)) expect(ids).toContain(id);
  }
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
  // A REAL 11-tab flow-grid state: flow-grid synthesizes exactly one zone per
  // tab, so 11 tabs -> zones 0..10, every one occupied. The arrangement is
  // distinctive rather than creation order: `t-known` was created FIRST but the
  // operator parked it in the last zone (10), and everyone else shifted down.
  const ids = ["t-known", ...tabIds(10)];
  const arranged: ZoneAssignments = { 10: "t-known" };
  tabIds(10).forEach((id, z) => (arranged[z] = id));

  it("fixture is a reachable state: 11 zones, all occupied, nothing out of range", () => {
    const zones = resolveLayout("flow-grid", ids.length).zones.length;
    expect(zones).toBe(11);
    expect(
      Object.keys(arranged)
        .map(Number)
        .sort((a, b) => a - b),
    ).toEqual(Array.from({ length: 11 }, (_, z) => z));
    // Reconcile at the current zone count is a no-op: this is a settled state.
    expect(reconcileAssignments(arranged, ids, zones)).toBe(arranged);
  });

  it("spawning the 12th tab keeps the tab at zone 10 at zone 10", async () => {
    // The real hook makes no layout call at all on this state...
    expect(await spawnOne(11, "flow-grid", arranged, ids)).toEqual([]);
    expect(computeQuickLaunchLayoutId("flow-grid", 11, false, 12)).toBeNull();
    // ...so the flow-grid simply regrows to 12 zones and the only assignment
    // pass is the hook's reconcile, which preserves every in-range assignment.
    const withNew = [...ids, "t-new"];
    const grownZones = resolveLayout("flow-grid", withNew.length).zones.length;
    expect(grownZones).toBe(12);
    const next = reconcileAssignments(arranged, withNew, grownZones);
    expect(next[10]).toBe("t-known");
    // The new tab lands in the new zone, not on top of anyone.
    expect(next[11]).toBe("t-new");
    for (let z = 0; z < 10; z++) expect(next[z]).toBe(arranged[z]);
  });

  it("anti-vacuity: the old shrink path WOULD have moved it", () => {
    // What the inline ladder's `setLayoutId("full-grid")` fed
    // `applyLayoutAssignments` before the fix: zones 9 and 10 do not exist in
    // a 9-zone layout and there is no hole to compact into, so `t-known` is
    // left unzoned — it loses zone 10. That is the scramble.
    const shrunk = applyLayoutAssignments(arranged, ids, 9);
    expect(shrunk[10]).toBeUndefined();
    expect(Object.values(shrunk)).not.toContain("t-known");
  });
});

describe("computeQuickLaunchLayoutId — grow-only", () => {
  it("never returns a layout with fewer zones than the current one", () => {
    // A full-grid (9 zones) with no empty zone but only 4 tabs after the spawn
    // (e.g. assignments not yet reconciled after closes): pickLayout(4) is
    // quad (4 zones) — a shrink, refused.
    expect(computeQuickLaunchLayoutId("full-grid", 9, false, 4)).toBeNull();
  });
});
