// @vitest-environment jsdom
/**
 * The trim hold across restore runs, with the real `useZoneLayout`,
 * `useZoneLabelsAndTags` and `useTerminalInitialization` wired the way a page
 * scope wires them: the layout hook publishes through a register callback, and
 * the restore effect depends on the object it publishes.
 *
 * These are the failures that only appear once all three run together, so the
 * reducers' own tests cannot see them.
 */

import { memo, useEffect, useMemo, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { fake } = vi.hoisted(() => ({
  fake: {
    invoke: (async () => ({ success: true, data: {} })) as (
      cmd: string,
      args?: { purpose?: string },
    ) => Promise<unknown>,
  },
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: { purpose?: string }) => fake.invoke(cmd, args),
  convertFileSrc: (s: string) => s,
  transformCallback: () => 0,
  isTauri: () => false,
  Channel: class {},
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: async () => () => {},
  once: async () => () => {},
  emit: async () => {},
  emitTo: async () => {},
}));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    label: "main",
    onCloseRequested: async () => () => {},
    listen: async () => () => {},
  }),
  getAllWindows: async () => [],
}));

import { instanceStorage } from "@/lib/instance-storage";
import { useZoneLayout, zoneLayoutStorageKey } from "./useZoneLayout";
import { useZoneLabelsAndTags } from "./useZoneLabelsAndTags";
import { useTerminalInitialization } from "./useTerminalInitialization";

const PAGE = "hold-test";
const NOOP = () => {};
const REFS = { current: new Map() };
const LAYOUT_STATE = {
  layoutId: "single",
  assignments: {},
  zoneLabels: {},
  zoneNotes: {},
  pinnedZones: new Set<number>(),
  focusedZone: 0,
};
const PERSISTENCE = {
  hasSavedLayout: () => false,
  getSavedLayout: () => null,
  clearSavedLayout: NOOP,
  saveSessionLayout: NOOP,
  saveScrollbackBuffers: async () => ({}),
  updateScrollbackPaths: NOOP,
};

type Tab = { id: string; isAlive: boolean };
type ScopeValue = {
  zone: ReturnType<typeof useZoneLayout>;
  labels: ReturnType<typeof useZoneLabelsAndTags>;
  setInitialized: (v: boolean) => void;
  tabs: Tab[];
  createTerminal: () => Promise<string>;
};
type Api = {
  zone: ScopeValue["zone"];
  tabs: Tab[];
  setTabs: (update: (tabs: Tab[]) => Tab[]) => void;
  createTerminal: () => Promise<string>;
};

let reconnectRuns = 0;
let reconnectIndeterminate = false;
const reconnect = async () => {
  reconnectRuns++;
  return reconnectIndeterminate ? null : [];
};

function Page({ value, pageId = PAGE }: { value: ScopeValue; pageId?: string }) {
  useTerminalInitialization({
    pageId,
    authenticated: true,
    tabs: value.tabs,
    terminalRefs: REFS,
    reconnectToExistingSessions: reconnect,
    createTerminal: value.createTerminal,
    createPlanTab: value.createTerminal,
    adoptWorkerTab: NOOP,
    setInitialized: value.setInitialized,
    updateTab: NOOP,
    zoneLayout: value.zone,
    labelsAndTags: value.labels,
    sessionPersistence: PERSISTENCE,
    layoutState: LAYOUT_STATE,
  } as never);
  return null;
}

let nextTab = 0;
const Scope = memo(function Scope({
  pageId = PAGE,
  register,
  api,
}: {
  pageId?: string;
  register: (v: ScopeValue) => void;
  api: { current: Api | null };
}) {
  const [initialized, setInitialized] = useState(false);
  const [tabs, setTabs] = useState<Tab[]>([]);
  const tabIds = useMemo(() => tabs.map((t) => t.id), [tabs]);
  const zone = useZoneLayout(tabIds, pageId, "main", undefined, undefined, initialized);
  const labels = useZoneLabelsAndTags(
    zone.layoutId,
    zone.assignments,
    pageId,
    zone.layoutSwitch,
    tabIds,
  );
  const createTerminal = useMemo(
    () => async () => {
      const id = `tab${nextTab++}`;
      setTabs((p) => [...p, { id, isAlive: true }]);
      return id;
    },
    [],
  );
  useEffect(() => {
    api.current = { zone, tabs, setTabs, createTerminal };
  });
  const value = useMemo(
    () => ({ zone, labels, setInitialized, tabs, createTerminal }),
    [zone, labels, tabs, createTerminal],
  );
  useEffect(() => {
    register(value);
  }, [register, value]);
  return null;
});

function Tree({ api }: { api: { current: Api | null } }) {
  const [published, setPublished] = useState<ScopeValue | null>(null);
  return (
    <>
      <Scope register={setPublished} api={api} />
      {published ? <Page value={published} /> : null}
    </>
  );
}

/** Two pages, as the provider mounts them: one restore hook follows the active. */
function TwoPages({
  a,
  b,
  active,
}: {
  a: { current: Api | null };
  b: { current: Api | null };
  active: { current: (page: string) => void };
}) {
  const [values, setValues] = useState<Record<string, ScopeValue>>({});
  const [on, setOn] = useState("A");
  useEffect(() => {
    active.current = setOn;
  }, [active]);
  const registerA = useMemo(() => (v: ScopeValue) => setValues((p) => ({ ...p, A: v })), []);
  const registerB = useMemo(() => (v: ScopeValue) => setValues((p) => ({ ...p, B: v })), []);
  return (
    <>
      <Scope pageId="A" register={registerA} api={a} />
      <Scope pageId="B" register={registerB} api={b} />
      {values[on] ? <Page value={values[on]} pageId={on} /> : null}
    </>
  );
}

const ROOM = { setup: 20_000 };

async function until(what: string, ready: () => boolean): Promise<void> {
  const deadline = Date.now() + ROOM.setup;
  while (!ready()) {
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 10));
  }
}
const settle = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Re-render the scope until `ready`. A render during an in-flight retry starts no other. */
async function renderUntil(what: string, target: { current: Api | null }, ready: () => boolean) {
  const deadline = Date.now() + ROOM.setup;
  while (!ready()) {
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    target.current?.setTabs((p) => [...p]);
    await settle(30);
  }
}

const record = (n: number, zone: number) => ({
  claudeSessionId: `1111111${n}-1111-4111-8111-111111111111`,
  terminalId: `old-${zone}`,
  workingDir: "/tmp",
  zoneIndex: zone,
  pageId: PAGE,
  title: `rec${zone}`,
});

let root: Root;
const api: { current: Api | null } = { current: null };

function mount(): Api {
  root = createRoot(document.createElement("div"));
  root.render(<Tree api={api} />);
  return api.current as unknown as Api;
}

beforeEach(() => {
  reconnectRuns = 0;
  reconnectIndeterminate = false;
  api.current = null;
  vi.spyOn(console, "warn").mockImplementation(NOOP);
  vi.spyOn(console, "error").mockImplementation(NOOP);
});
afterEach(async () => {
  root.unmount();
  vi.restoreAllMocks();
  instanceStorage.removeItem?.(zoneLayoutStorageKey(PAGE, "main"));
  await settle(20);
});

function saveLayout(origin: "auto" | "operator", page = PAGE, layoutId = "six-pack") {
  instanceStorage.setJSON(zoneLayoutStorageKey(page, "main"), {
    layoutId,
    assignments: {},
    focusedZone: 0,
    layoutOrigin: origin,
  });
}

describe("restore runs and the trim hold", () => {
  it("does not re-run a deferred restore in a loop while the drain lasts", async () => {
    fake.invoke = async (cmd) => {
      await settle(5);
      if (cmd === "terminal_session_list_open") {
        return { success: true, data: { sessions: [], deferredByDrain: { reason: "drain" } } };
      }
      return { success: true, data: { settled: true, held: [] } };
    };
    mount();
    await until("the first run", () => reconnectRuns >= 1);
    await settle(800);
    // A publish or two can retry it once. An unbroken loop runs it dozens of times.
    expect(reconnectRuns).toBeLessThanOrEqual(3);
  });

  it("gives a tab a zone that exists when the grid trimmed before the re-run", async () => {
    let mode: "deferred" | "lifted" = "deferred";
    fake.invoke = async (cmd) => {
      await settle(5);
      if (cmd === "terminal_session_list_open") {
        return mode === "deferred"
          ? { success: true, data: { sessions: [], deferredByDrain: { reason: "drain" } } }
          : { success: true, data: { sessions: [record(5, 5)], deferredByDrain: null } };
      }
      return { success: true, data: { settled: true, held: [] } };
    };
    saveLayout("auto");
    mount();
    await until("the deferred run", () => reconnectRuns >= 1 && api.current !== null);
    for (let i = 0; i < 4; i++) {
      await api.current?.createTerminal();
      await settle(20);
    }
    api.current?.setTabs((p) => p.slice(0, 3));
    await until("the trim", () => api.current?.zone.layoutId === "quad");
    mode = "lifted";
    // Any scope render retries a deferred page, which is how the lift lands here.
    await renderUntil("the restored tab", api, () => (api.current?.tabs.length ?? 0) === 4);
    await settle(100);
    const zone = api.current?.zone;
    const zones = zone?.layout.zones.length ?? 0;
    const placed = Object.entries(zone?.assignments ?? {});
    expect(placed).toHaveLength(4);
    for (const [index] of placed) expect(Number(index)).toBeLessThan(zones);
  });

  /** A's merging re-run is in flight when the operator switches to B. */
  async function runAcrossSwitch(
    gridA: string,
    gridB: string,
    recordZone: number,
    tabs: number,
    closeOffScreen = 0,
  ) {
    let mode: "deferred" | "lifted" = "deferred";
    let blocked = false;
    let release = NOOP;
    fake.invoke = async (cmd, args) => {
      await settle(5);
      if (cmd === "terminal_session_list_open" && args?.purpose === "restore") {
        if (mode === "deferred") {
          return { success: true, data: { sessions: [], deferredByDrain: { reason: "drain" } } };
        }
        if (!blocked) {
          blocked = true;
          await new Promise<void>((r) => (release = r));
        }
        return {
          success: true,
          data: { sessions: [{ ...record(5, recordZone), pageId: "A" }], deferredByDrain: null },
        };
      }
      return { success: true, data: { settled: true, held: [] } };
    };
    saveLayout("auto", "A", gridA);
    saveLayout("auto", "B", gridB);
    const a: { current: Api | null } = { current: null };
    const b: { current: Api | null } = { current: null };
    const active: { current: (page: string) => void } = { current: NOOP };
    root = createRoot(document.createElement("div"));
    root.render(<TwoPages a={a} b={b} active={active} />);
    await until("page A's first run", () => reconnectRuns >= 1 && a.current !== null);
    for (let i = 0; i < tabs; i++) {
      await a.current?.createTerminal();
      await settle(20);
    }
    mode = "lifted";
    await renderUntil("the re-run's read", a, () => blocked);
    active.current("B");
    await settle(100);
    if (closeOffScreen > 0) {
      a.current?.setTabs((p) => p.slice(0, p.length - closeOffScreen));
      await until("the off-screen trim", () => a.current?.zone.layoutId !== gridA);
    }
    release();
    await until(
      "the restored tab",
      () => (a.current?.tabs.length ?? 0) === tabs - closeOffScreen + 1,
    );
    await settle(100);
    return a.current?.zone;
  }

  it("a run that outlives a page switch keeps a free recorded zone on its own page", async () => {
    const zone = await runAcrossSwitch("six-pack", "single", 4, 2);
    expect(zone?.layoutId).toBe("six-pack");
    expect(zone?.assignments[4]).toBeDefined();
  }, 30_000);

  it("a run that outlives a page switch checks its own page's grid, not the one on screen", async () => {
    const zone = await runAcrossSwitch("quad", "six-pack", 5, 3);
    expect(zone?.layoutId).toBe("quad");
    expect(
      Object.keys(zone?.assignments ?? {})
        .map(Number)
        .sort(),
    ).toEqual([0, 1, 2, 3]);
  }, 30_000);

  it("a run sees a trim on its page after the operator has left that page", async () => {
    const zone = await runAcrossSwitch("six-pack", "single", 5, 4, 1);
    expect(zone?.layoutId).toBe("quad");
    expect(
      Object.keys(zone?.assignments ?? {})
        .map(Number)
        .sort(),
    ).toEqual([0, 1, 2, 3]);
  }, 30_000);

  it("holds the trim after a deferred retry aborts, until a later run opens it", async () => {
    let mode: "deferred" | "lifted" = "deferred";
    fake.invoke = async (cmd) => {
      await settle(5);
      if (cmd === "terminal_session_list_open") {
        return mode === "deferred"
          ? { success: true, data: { sessions: [], deferredByDrain: { reason: "drain" } } }
          : { success: true, data: { sessions: [], deferredByDrain: null } };
      }
      return { success: true, data: { settled: true, held: [] } };
    };
    saveLayout("auto");
    mount();
    await until("the deferred run", () => reconnectRuns >= 1 && api.current !== null);
    for (let i = 0; i < 3; i++) {
      await api.current?.createTerminal();
      await settle(20);
    }
    reconnectIndeterminate = true;
    const before = reconnectRuns;
    await renderUntil("the aborted retry", api, () => reconnectRuns > before);
    await settle(50);
    api.current?.setTabs((p) => p.slice(0, 2));
    await settle(500);
    expect(api.current?.zone.layoutId).toBe("six-pack");

    reconnectIndeterminate = false;
    mode = "lifted";
    await renderUntil(
      "the trim the hold deferred",
      api,
      () => api.current?.zone.layoutId !== "six-pack",
    );
  }, 30_000);

  it("keeps the hold when an earlier run's drain timer fires during a later run", async () => {
    let restoreReads = 0;
    let scrollbackCleanups = 0;
    let finishSecondRead = NOOP;
    fake.invoke = async (cmd, args) => {
      await settle(5);
      if (cmd === "terminal_holder_sweep_status") {
        return {
          success: true,
          data: { settled: false, held: restoreReads === 0 ? ["old-5"] : [] },
        };
      }
      if (cmd === "terminal_cleanup_scrollback") scrollbackCleanups++;
      if (cmd === "terminal_session_list_open" && args?.purpose === "restore") {
        restoreReads++;
        if (restoreReads >= 2) await new Promise<void>((r) => (finishSecondRead = r));
        return {
          success: true,
          data: {
            sessions: [record(1, 0), record(2, 1), record(3, 2), record(5, 5)],
            deferredByDrain: null,
          },
        };
      }
      return { success: true, data: { settled: true, held: [] } };
    };
    saveLayout("auto");
    mount();
    await until("the re-run to start", () => restoreReads >= 2);
    await until("the first run's tabs", () => (api.current?.tabs.length ?? 0) >= 3);
    api.current?.setTabs((p) => p.slice(0, 2));
    await until("the first run's drain timer", () => scrollbackCleanups >= 1);
    // Room for the timer's release to reach a trim, if it wrongly releases.
    await settle(750);
    expect(api.current?.zone.layoutId).toBe("six-pack");

    finishSecondRead();
    await until("the trim the hold deferred", () => api.current?.zone.layoutId !== "six-pack");
  }, 30_000);
});
