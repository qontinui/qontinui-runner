/**
 * Auto-trim, driven through `useZoneLayout` itself (the shared hooks
 * harness), so what is pinned is the effect's ordering against the reconcile,
 * auto-grow, maximize and compaction effects — not the pure reducer alone
 * (`zoneAutoTrim.test.ts` covers that).
 *
 * The contract: a page whose layout the RUNNER chose (`layoutOrigin: "auto"`)
 * settles at `pickLayout(tabCount)` after a tab-count decrease, once no hold
 * is active. An operator's layout, a page saved before the origin existed,
 * mount, the restore drain, a maximized zone, a pending compaction, tmux mode
 * and a page popped out of this window never trim.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("react", async () => {
  const { hooksHarness } = await import("@/lib/__test-helpers__/hooks-harness");
  return {
    ...hooksHarness.reactMock,
    // `TerminalPageContext` (for `pageKey`) creates a context at import.
    createContext: () => ({ Provider: null }),
    useContext: () => "default",
  };
});

const store = vi.hoisted(() => new Map<string, unknown>());
vi.mock("@/lib/instance-storage", () => ({
  instanceStorage: {
    getJSON: (key: string, fallback: unknown) => (store.has(key) ? store.get(key) : fallback),
    setJSON: (key: string, value: unknown) => {
      store.set(key, value);
    },
  },
}));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: async () => ({ label: "term-2" }),
}));
vi.mock("@/lib/pageBindings", () => ({ setPageBinding: () => {} }));

import { hooksHarness as harness } from "@/lib/__test-helpers__/hooks-harness";
import { useZoneLabelsAndTags } from "./useZoneLabelsAndTags";
import { useZoneLayout, zoneLayoutStorageKey, type ZoneAssignments } from "./useZoneLayout";
import { useTerminalWindowActions } from "./useTerminalWindowActions";
import { AUTO_TRIM_STORAGE_KEY, migrateZoneMetadataByTab, type LayoutOrigin } from "./zoneAutoTrim";

const PAGE = "trim-test";

const ids = (n: number) => Array.from({ length: n }, (_, i) => `t${i}`);

/** Dense assignments: tab `ti` in zone `i`. */
function dense(tabIds: string[]): ZoneAssignments {
  return Object.fromEntries(tabIds.map((id, i) => [i, id]));
}

const without = (tabIds: string[], ...closed: string[]) =>
  tabIds.filter((id) => !closed.includes(id));

interface MountOptions {
  origin?: LayoutOrigin;
  initialized?: boolean;
  windowLabel?: string;
  /** Seed nothing: the window's key was written by someone else (pop-out). */
  seed?: boolean;
}

/**
 * Mount `useZoneLayout` (and the labels hook beside it, as the page provider
 * does) on a persisted layout. The returned render takes the next live tab
 * list; `setInitialized` flips the restore-settled flag for the next render.
 */
function mount(
  layoutId: string,
  tabIds: string[],
  assignments: ZoneAssignments,
  opts: MountOptions = {},
) {
  harness.reset();
  const windowLabel = opts.windowLabel ?? "main";
  if (opts.seed !== false) {
    store.set(zoneLayoutStorageKey(PAGE, windowLabel), {
      layoutId,
      assignments,
      focusedZone: 0,
      ...(opts.origin ? { layoutOrigin: opts.origin } : {}),
    });
  }
  let live = tabIds;
  let initialized = opts.initialized ?? true;
  function Host() {
    harness.beginRender();
    const zone = useZoneLayout(live, PAGE, windowLabel, undefined, undefined, initialized);
    const labels = useZoneLabelsAndTags(
      zone.layoutId,
      zone.assignments,
      PAGE,
      zone.layoutSwitchOrigin,
    );
    return { ...zone, labels };
  }
  const render = (next?: string[]) => {
    if (next) live = next;
    return harness.renderSettled(Host);
  };
  return Object.assign(render, {
    setInitialized(v: boolean) {
      initialized = v;
    },
  });
}

const persistedOrigin = (windowLabel = "main") =>
  (store.get(zoneLayoutStorageKey(PAGE, windowLabel)) as { layoutOrigin?: string } | undefined)
    ?.layoutOrigin;

beforeEach(() => {
  store.clear();
  harness.reset();
});

describe("useZoneLayout auto-trim: what trims", () => {
  it("an auto six-pack closing to 2 tabs settles at split", () => {
    const before = ids(6);
    const render = mount("six-pack", before, dense(before), { origin: "auto" });
    render();
    const r = render(["t0", "t1"]);
    expect(r.layoutId).toBe("split");
    expect(r.assignments).toStrictEqual({ 0: "t0", 1: "t1" });
    expect(r.layoutOrigin).toBe("auto");
  });

  it("survivors in high zones are compacted into the trimmed grid, none hidden", () => {
    const before = ids(6);
    const render = mount("six-pack", before, dense(before), { origin: "auto" });
    render();
    const r = render(["t1", "t5"]);
    expect(r.layoutId).toBe("split");
    expect(r.assignments).toStrictEqual({ 0: "t5", 1: "t1" });
    expect(r.unassignedTabIds).toEqual([]);
  });

  it("a burst close settles at pickLayout(final count)", () => {
    const before = ids(9);
    const render = mount("full-grid", before, dense(before), { origin: "auto" });
    render();
    // One tab per render, as a batch close that lands over several renders.
    let live = before;
    for (const id of ["t8", "t7", "t6", "t5", "t4", "t3", "t2"]) {
      live = without(live, id);
      render(live);
    }
    expect(render().layoutId).toBe("split");
    // And several in one render.
    const all = mount("full-grid", before, dense(before), { origin: "auto" });
    all();
    expect(all(["t0", "t1", "t2"]).layoutId).toBe("quad");
  });

  it("an auto flow-grid at 9 tabs or fewer trims to full-grid", () => {
    const before = ids(12);
    const render = mount("flow-grid", before, dense(before), { origin: "auto" });
    expect(render().layout.zones).toHaveLength(12);
    const after = without(before, "t1", "t4", "t7");
    const r = render(after);
    expect(r.layoutId).toBe("full-grid");
    expect(r.layout.zones).toHaveLength(9);
    // Every survivor is drawn: in-range zones kept, t9..t11 compacted in.
    expect(Object.values(r.assignments).sort()).toStrictEqual([...after].sort());
    for (const id of ["t0", "t2", "t3", "t5", "t6", "t8"]) {
      expect(r.assignments[Number(id.slice(1))]).toBe(id);
    }
  });

  it("growth after a trim still works, and nothing thrashes", () => {
    const before = ids(6);
    const render = mount("six-pack", before, dense(before), { origin: "auto" });
    render();
    expect(render(["t0", "t1"]).layoutId).toBe("split");
    // `renderSettled` throws if the effects ping-pong without settling.
    expect(render(["t0", "t1", "n1"]).layoutId).toBe("quad");
    expect(render(["t0", "t1", "n1", "n2", "n3"]).layoutId).toBe("six-pack");
    expect(render(["t0", "t1", "n1"]).layoutId).toBe("quad");
  });

  it("auto-grow turns an operator page into an auto one, which then trims", () => {
    const render = mount("split", ["a", "b"], { 0: "a", 1: "b" }, { origin: "operator" });
    render();
    const grown = render(["a", "b", "c", "d", "e"]);
    expect(grown.layoutId).toBe("six-pack");
    expect(grown.layoutOrigin).toBe("auto");
    expect(render(["a", "b"]).layoutId).toBe("split");
  });
});

describe("useZoneLayout auto-trim: what does not trim", () => {
  it("an operator quad at 2 tabs is untouched", () => {
    const render = mount(
      "quad",
      ["a", "b", "c"],
      { 0: "a", 1: "b", 2: "c" },
      {
        origin: "operator",
      },
    );
    render();
    expect(render(["a", "b"]).layoutId).toBe("quad");
  });

  it("a persisted page with no layoutOrigin is untouched", () => {
    const before = ids(6);
    const render = mount("six-pack", before, dense(before));
    const r0 = render();
    expect(r0.layoutOrigin).toBeUndefined();
    expect(render(["t0", "t1"]).layoutId).toBe("six-pack");
    expect(persistedOrigin()).toBeUndefined();
  });

  it("mount with a persisted six-pack and fewer tabs does not trim", () => {
    const render = mount("six-pack", ["a", "b"], { 0: "a", 1: "b" }, { origin: "auto" });
    expect(render().layoutId).toBe("six-pack");
    // A later increase is not a decrease either.
    expect(render(["a", "b", "c"]).layoutId).toBe("six-pack");
  });

  it("an operator flow-grid keeps today's behaviour: holes, no re-pack", () => {
    const before = ids(12);
    const render = mount("flow-grid", before, dense(before), { origin: "operator" });
    render();
    const r = render(without(before, "t1", "t4", "t7"));
    expect(r.layoutId).toBe("flow-grid");
    expect(r.layout.zones).toHaveLength(12);
  });

  it("the main window's tabIds dropping to 0 (page pop-out) does not trim", () => {
    const before = ids(6);
    const render = mount("six-pack", before, dense(before), { origin: "auto" });
    render();
    const r = render([]);
    expect(r.layoutId).toBe("six-pack");
    // Re-docked: the grid is still the one it had.
    expect(render(before).layoutId).toBe("six-pack");
  });

  it("the preference off is exactly grow-only", () => {
    store.set(AUTO_TRIM_STORAGE_KEY, false);
    const render = mount("split", ["a", "b"], { 0: "a", 1: "b" }, { origin: "auto" });
    render();
    expect(render(["a", "b", "c", "d", "e"]).layoutId).toBe("six-pack");
    expect(render(["a", "b"]).layoutId).toBe("six-pack");
    expect(render(["a"]).layoutId).toBe("six-pack");
  });

  it("the preference is read when the trim applies, so a flip needs no remount", () => {
    store.set(AUTO_TRIM_STORAGE_KEY, false);
    const before = ids(6);
    const render = mount("six-pack", before, dense(before), { origin: "auto" });
    render();
    expect(render(["t0", "t1", "t2", "t3"]).layoutId).toBe("six-pack");
    store.set(AUTO_TRIM_STORAGE_KEY, true);
    expect(render(["t0", "t1"]).layoutId).toBe("split");
  });
});

describe("useZoneLayout auto-trim: holds", () => {
  it("a decrease while the restore is unsettled waits, then applies when it settles", () => {
    const before = ids(6);
    const render = mount("six-pack", before, dense(before), {
      origin: "auto",
      initialized: false,
    });
    render();
    expect(render(["t0", "t1"]).layoutId).toBe("six-pack");
    render.setInitialized(true);
    expect(render().layoutId).toBe("split");
  });

  it("the trim applies from the count when the hold releases, not the one that latched it", () => {
    const before = ids(6);
    const render = mount("six-pack", before, dense(before), {
      origin: "auto",
      initialized: false,
    });
    render();
    render(["t0", "t1"]);
    // A tab arrives before the restore settles: 3 tabs → quad, not split.
    render(["t0", "t1", "t2"]);
    render.setInitialized(true);
    expect(render().layoutId).toBe("quad");
  });

  it("a decrease while a zone is maximized waits, and applies when maximize clears", () => {
    const before = ids(6);
    const render = mount("six-pack", before, dense(before), { origin: "auto" });
    render().setMaximizedZone(0);
    const held = render(["t0", "t1"]);
    expect(held.layoutId).toBe("six-pack");
    expect(held.maximizedZone).toBe(0);
    held.setMaximizedZone(null);
    expect(render().layoutId).toBe("split");
  });

  it("closing the maximized tab itself un-maximizes, and then the trim applies", () => {
    const before = ids(6);
    const render = mount("six-pack", before, dense(before), { origin: "auto" });
    render().setMaximizedZone(4);
    const r = render(["t0", "t1"]);
    expect(r.maximizedZone).toBeNull();
    expect(r.layoutId).toBe("split");
  });

  it("a decrease while a compaction is pending does not trim; the compaction decides", () => {
    const all = ["a1", "s1", "a2", "x", "y", "z"];
    const render = mount("six-pack", all, dense(all), { origin: "auto" });
    render().requestCompaction(["a1", "a2"], ["s1"]);
    // A different tab closes while `s1` is still in the list.
    const held = render(["a1", "s1", "a2", "y", "z"]);
    expect(held.layoutId).toBe("six-pack");
    const r = render(["a1", "a2", "y", "z"]);
    expect(r.layoutId).toBe("quad");
    expect(r.assignments).toStrictEqual({ 0: "a1", 1: "a2", 2: "y", 3: "z" });
    expect(r.layoutOrigin).toBe("auto");
  });
});

describe("useZoneLayout auto-trim: origin", () => {
  it("setLayoutId records operator and auto, and restore keeps what is stored", () => {
    const render = mount("quad", ["a", "b"], { 0: "a", 1: "b" }, { origin: "auto" });
    render().setLayoutId("six-pack", { source: "restore" });
    expect(render().layoutOrigin).toBe("auto");
    render().setLayoutId("quad", { source: "operator" });
    expect(render().layoutOrigin).toBe("operator");
    render().setLayoutId("full-grid", { source: "restore" });
    expect(render().layoutOrigin).toBe("operator");
    render().setLayoutId("six-pack", { source: "auto" });
    expect(render().layoutOrigin).toBe("auto");
    expect(persistedOrigin()).toBe("auto");
  });

  it("a boot restore keeps an auto page auto, so it still trims after the restart", () => {
    const before = ids(6);
    const render = mount("quad", before, dense(before.slice(0, 4)), { origin: "auto" });
    render().setLayoutId("six-pack", { source: "restore" });
    const r = render(["t0", "t1"]);
    expect(r.layoutOrigin).toBe("auto");
    expect(r.layoutId).toBe("split");
  });

  it("the pop-out page copy carries layoutOrigin, so the pop-out trims too", async () => {
    const before = ids(6);
    mount("six-pack", before, dense(before), { origin: "auto" })();
    expect(persistedOrigin("main")).toBe("auto");

    const label = await useTerminalWindowActions().popOutPage(PAGE);
    expect(label).toBe("term-2");
    expect(persistedOrigin("term-2")).toBe("auto");

    const popped = mount("single", before, {}, { windowLabel: "term-2", seed: false });
    expect(popped().layoutOrigin).toBe("auto");
    expect(popped(["t0", "t1"]).layoutId).toBe("split");
  });
});

describe("useZoneLayout auto-trim: tmux work mode", () => {
  it("a close in tmux does not trim; the toggle back to the grid applies the owed trim", () => {
    const before = ids(6);
    const render = mount("six-pack", before, dense(before), { origin: "auto" });
    render().enterTmuxMode();
    const inTmux = render(["t0", "t1"]);
    expect(inTmux.workMode).toBe("tmux");
    expect(inTmux.layoutId).toBe("single");
    expect(inTmux.gridView.layoutId).toBe("six-pack");

    inTmux.exitTmuxMode();
    const r = render();
    expect(r.workMode).toBe("grid");
    expect(r.layoutId).toBe("split");
    expect(r.layoutOrigin).toBe("auto");
    expect(Object.values(r.assignments).sort()).toStrictEqual(["t0", "t1"]);
  });

  it("an operator preset pick out of tmux is the operator's, and is not trimmed", () => {
    const before = ids(6);
    const render = mount("six-pack", before, dense(before), { origin: "auto" });
    render().enterTmuxMode();
    render(["t0", "t1", "t2"]).setLayoutId("full-grid", { source: "operator" });
    const r = render();
    expect(r.workMode).toBe("grid");
    expect(r.layoutId).toBe("full-grid");
    expect(r.layoutOrigin).toBe("operator");
    expect(render(["t0", "t1"]).layoutId).toBe("full-grid");
  });
});

describe("useZoneLayout auto-trim: metadata follows the tab (D5)", () => {
  it("after a trim, labels, notes, pins and filters sit on the same tabs as before", () => {
    const before = ids(6);
    const render = mount("six-pack", before, dense(before), { origin: "auto" });
    const r0 = render();
    r0.labels.setZoneLabel(1, "docs");
    r0.labels.setZoneLabel(5, "deploy");
    r0.labels.setZoneLabel(3, "closing");
    r0.labels.setZoneNote(5, "watch the canary");
    r0.labels.togglePin(5);
    const pre = render();
    // Output filters are ZoneGrid reducer state, migrated with the same
    // function over the grid's assignments on every assignment change.
    const filtersBefore = { 1: "error", 5: "WARN" };

    const r = render(["t1", "t5"]);
    expect(r.layoutId).toBe("split");
    expect(r.assignments).toStrictEqual({ 0: "t5", 1: "t1" });
    const zoneOf = (tab: string) =>
      Number(Object.entries(r.assignments).find(([, id]) => id === tab)![0]);
    expect(r.labels.zoneLabels).toStrictEqual({ [zoneOf("t1")]: "docs", [zoneOf("t5")]: "deploy" });
    expect(r.labels.zoneNotes).toStrictEqual({ [zoneOf("t5")]: "watch the canary" });
    expect([...r.labels.pinnedZones]).toStrictEqual([zoneOf("t5")]);

    // The filters' migration: across the close (t3 gone) and then the trim.
    const afterClose = { 0: "t0", 1: "t1", 2: "t2", 4: "t4", 5: "t5" };
    const ignored = migrateZoneMetadataByTab(filtersBefore, pre.gridView.assignments, afterClose);
    const filters = migrateZoneMetadataByTab(ignored, afterClose, r.gridView.assignments);
    expect(filters).toStrictEqual({ [zoneOf("t1")]: "error", [zoneOf("t5")]: "WARN" });
  });

  it("an operator switch keeps today's per-layout path: labels are not carried by tab", () => {
    const render = mount("quad", ["a", "b"], { 0: "a", 3: "b" }, { origin: "operator" });
    render().labels.setZoneLabel(3, "b-label");
    render().setLayoutId("split", { source: "operator" });
    const r = render();
    expect(r.layoutSwitchOrigin).toBe("operator");
    expect(r.assignments).toStrictEqual({ 0: "a", 1: "b" });
    // b moved to zone 1; its label did not follow (an auto switch would
    // have put it on zone 1).
    expect(r.labels.zoneLabels).toStrictEqual({ 3: "b-label" });
  });

  it("an auto switch carries the same label by tab", () => {
    const render = mount("quad", ["a", "b"], { 0: "a", 3: "b" }, { origin: "auto" });
    render().labels.setZoneLabel(3, "b-label");
    render().setLayoutId("split", { source: "auto" });
    const r = render();
    expect(r.assignments).toStrictEqual({ 0: "a", 1: "b" });
    expect(r.labels.zoneLabels).toStrictEqual({ 1: "b-label" });
  });
});
