// @vitest-environment jsdom
/**
 * The page roster resyncs across the windows of one runner process (plan
 * 2026-11-23-runner-terminal-page-roster-resyncs-across-windows, Phase 1).
 * Each window mounts the hook in its own React root under its own Tauri
 * label; the `emit` mock delivers every event to EVERY registered listener,
 * the sender's included (Tauri v2 `emit` semantics), so the own-echo filter
 * is actually exercised.
 */

import { StrictMode, act, createElement, startTransition } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

type Listener = (event: { event: string; payload: unknown }) => void;

const { bus, windowLabel } = vi.hoisted(() => ({
  bus: {
    /** A LIST of listeners per event: every window registers one. */
    listeners: new Map<string, Listener[]>(),
    /** Every event emitted, in order. */
    emitted: [] as Array<{ event: string; payload: unknown }>,
    /** While set, `listen` registrations of this event wait for `release`. */
    deferEvent: null as string | null,
    release: [] as Array<() => void>,
    /** While true, emits queue in `held` instead of dispatching. */
    hold: false,
    held: [] as Array<{ event: string; payload: unknown }>,
  },
  windowLabel: { current: "main" },
}));

function dispatch(event: string, payload: unknown) {
  for (const cb of [...(bus.listeners.get(event) ?? [])]) cb({ event, payload });
}

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ label: windowLabel.current }),
}));
vi.mock("@tauri-apps/api/core", () => ({
  // Every backend read fails: reconcile is best-effort and adds nothing.
  invoke: vi.fn(() => Promise.reject(new Error("no backend in test"))),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (event: string, cb: Listener) => {
    if (bus.deferEvent === event) await new Promise<void>((r) => bus.release.push(r));
    const list = bus.listeners.get(event) ?? [];
    list.push(cb);
    bus.listeners.set(event, list);
    return () => {
      const cur = bus.listeners.get(event) ?? [];
      bus.listeners.set(
        event,
        cur.filter((c) => c !== cb),
      );
    };
  }),
  emit: vi.fn((event: string, payload: unknown) => {
    bus.emitted.push({ event, payload });
    if (bus.hold) bus.held.push({ event, payload });
    else dispatch(event, payload);
    return Promise.resolve();
  }),
}));

import { instanceStorage } from "@/lib/instance-storage";
import { setPageBindings } from "@/lib/pageBindings";
import { TERMINAL_PAGES_SAVED_EVENT, rosterDigest, rosterEqual } from "./terminalPagesSync";
import { PAGES_STORAGE_KEY, useTerminalPages, type TerminalPageConfig } from "./useTerminalPages";

type Hook = ReturnType<typeof useTerminalPages>;

interface MountedWindow {
  hook: () => Hook;
  renders: () => number;
  unmount: () => void;
}

const mounted: MountedWindow[] = [];

const ROSTER: TerminalPageConfig[] = [
  { id: "p1", name: "Alpha", createdAt: 1, origin: "operator" },
  { id: "p2", name: "Beta", createdAt: 2, origin: "operator" },
  { id: "p3", name: "Gamma", createdAt: 3, origin: "operator" },
];

const persisted = () => instanceStorage.getJSON<TerminalPageConfig[]>(PAGES_STORAGE_KEY, []);
const names = (pages: TerminalPageConfig[]) => pages.map((p) => p.name);
const ids = (pages: TerminalPageConfig[]) => pages.map((p) => p.id);
const savedEvents = () => bus.emitted.filter((e) => e.event === TERMINAL_PAGES_SAVED_EVENT);

/** Let every awaited promise in the mount (listen, reconcile) resolve. */
async function settle() {
  for (let i = 0; i < 5; i++) {
    await act(() => new Promise((r) => setTimeout(r, 0)));
  }
}

/**
 * Mount the hook as the window `label`, in its own root (the `mountPopOut`
 * pattern of `useTerminalPages.prune.test.tsx`, generalized to any label and
 * to a page-pinned pop-out via `?page=<id>`).
 */
async function mountWindow(
  label: string,
  pinnedPage?: string,
  strict = false,
): Promise<MountedWindow> {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  let latest: Hook | null = null;
  let renders = 0;
  function Win() {
    renders++;
    latest = useTerminalPages();
    return null;
  }
  const prevUrl = window.location.href;
  windowLabel.current = label;
  if (pinnedPage) window.history.replaceState({}, "", `?page=${pinnedPage}`);
  try {
    await act(async () =>
      root.render(strict ? createElement(StrictMode, null, createElement(Win)) : createElement(Win)),
    );
    await settle();
  } finally {
    windowLabel.current = "main";
    if (pinnedPage) window.history.replaceState({}, "", prevUrl);
  }
  const win: MountedWindow = {
    hook: () => latest!,
    renders: () => renders,
    unmount: () => {
      act(() => root.unmount());
      container.remove();
    },
  };
  mounted.push(win);
  return win;
}

/** Make `localStorage.setItem` throw a full-storage error for instance key `key`. */
function failWritesTo(key: string) {
  const realSetItem = Storage.prototype.setItem;
  vi.spyOn(Storage.prototype, "setItem").mockImplementation(function (
    this: Storage,
    raw: string,
    value: string,
  ) {
    if (raw === key || raw.startsWith(`${key}:`)) {
      throw new DOMException("storage is full", "QuotaExceededError");
    }
    realSetItem.call(this, raw, value);
  });
}

/** Deliver an event that ANOTHER window (`source`) sent, as Tauri would. */
function receiveFrom(source: string, digest: string) {
  act(() => dispatch(TERMINAL_PAGES_SAVED_EVENT, { source, digest }));
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  localStorage.clear();
  setPageBindings({});
  bus.listeners.clear();
  bus.emitted.length = 0;
  bus.held.length = 0;
  bus.hold = false;
  bus.deferEvent = null;
  bus.release.length = 0;
  windowLabel.current = "main";
  instanceStorage.setJSON(PAGES_STORAGE_KEY, ROSTER);
  instanceStorage.setItem("qontinui-terminal-active-page", "p1");
});

afterEach(() => {
  vi.useRealTimers();
  for (const w of mounted.splice(0)) w.unmount();
  localStorage.clear();
  vi.restoreAllMocks();
});

describe("rosterDigest / rosterEqual", () => {
  it("are insensitive to key order and sensitive to every field and to order", () => {
    const a = [{ id: "p1", name: "A", createdAt: 1 }];
    const reordered = [{ name: "A", createdAt: 1, id: "p1" }];
    expect(rosterDigest(a)).toBe(rosterDigest(reordered));
    expect(rosterEqual(a, reordered)).toBe(true);

    expect(rosterEqual(a, [{ ...a[0], defaultWorkingDir: "/x" }])).toBe(false);
    expect(rosterDigest(a)).not.toBe(rosterDigest([{ ...a[0], defaultWorkingDir: "/x" }]));
    expect(rosterEqual(ROSTER, [ROSTER[1], ROSTER[0], ROSTER[2]])).toBe(false);
    expect(rosterEqual(ROSTER, ROSTER.slice(0, 2))).toBe(false);
    expect(
      rosterEqual(
        ROSTER,
        ROSTER.map((p) => ({ ...p })),
      ),
    ).toBe(true);
    expect(rosterEqual([], [])).toBe(true);
  });
});

describe("a write in one window reaches the others", () => {
  it("a rename in the pop-out shows in main without main writing", async () => {
    const main = await mountWindow("main");
    const pop = await mountWindow("term-2");
    const mainSaves = () =>
      savedEvents().filter((e) => (e.payload as { source: string }).source === "main");

    act(() => void pop.hook().renamePage("p2", "Renamed"));

    expect(names(persisted())).toEqual(["Alpha", "Renamed", "Gamma"]);
    expect(names(main.hook().allPages)).toEqual(["Alpha", "Renamed", "Gamma"]);
    expect(mainSaves()).toHaveLength(0);
  });

  it("a page main adds shows in the pop-out", async () => {
    const main = await mountWindow("main");
    const pop = await mountWindow("term-2");

    let added = "";
    act(() => {
      added = main.hook().addPage("Delta");
    });

    expect(ids(pop.hook().allPages)).toEqual(["p1", "p2", "p3", added]);
    expect(names(pop.hook().allPages)).toEqual(["Alpha", "Beta", "Gamma", "Delta"]);
  });

  it("a pinned pop-out shows its page's new name after main renames it", async () => {
    const main = await mountWindow("main");
    const pinned = await mountWindow("term-3", "p2");
    expect(pinned.hook().isPinned).toBe(true);
    expect(names(pinned.hook().pages)).toEqual(["Beta"]);

    act(() => void main.hook().renamePage("p2", "Renamed"));

    expect(names(pinned.hook().pages)).toEqual(["Renamed"]);
  });

  it("three windows: a write in one reaches both others", async () => {
    const main = await mountWindow("main");
    const popA = await mountWindow("term-2");
    const popB = await mountWindow("term-3");

    act(() => void popA.hook().renamePage("p3", "Third"));

    expect(names(main.hook().allPages)).toEqual(["Alpha", "Beta", "Third"]);
    expect(names(popB.hook().allPages)).toEqual(["Alpha", "Beta", "Third"]);
  });

  it("two saves in a row: the receiver ends on the second roster", async () => {
    const main = await mountWindow("main");
    const pop = await mountWindow("term-2");

    act(() => void pop.hook().renamePage("p1", "First"));
    act(() => void pop.hook().renamePage("p1", "Second"));

    expect(names(main.hook().allPages)).toEqual(["Second", "Beta", "Gamma"]);
    // The receiver's synchronous roster (the rename refusal reads it) follows
    // too, once the reload commits (the `allPagesRef` sync effect).
    expect(main.hook().renamePage("p2", "Second")).toBe(false);
  });

  it("removing the receiver's active page elsewhere moves it to its first visible page", async () => {
    instanceStorage.setItem("qontinui-terminal-active-page", "p2");
    const main = await mountWindow("main");
    expect(main.hook().activePageId).toBe("p2");
    const pop = await mountWindow("term-2");

    await act(async () => {
      await pop.hook().removePage("p2");
    });
    await settle();

    expect(ids(main.hook().allPages)).toEqual(["p1", "p3"]);
    expect(main.hook().activePageId).toBe("p1");
  });
});

describe("the receiver's branches", () => {
  it("a reload never replaces this window's own write queued earlier in the same batch", async () => {
    const main = await mountWindow("main");

    act(() => {
      // A pending update first, so the next one is queued, not run eagerly.
      void main.hook().renamePage("p1", "Renamed");
      main.hook().addPage("Queued");
      // Another window's event arrives before main renders: the store it reads
      // holds the rename but not yet the queued page.
      expect(persisted()).toHaveLength(3);
      dispatch(TERMINAL_PAGES_SAVED_EVENT, { source: "term-2", digest: rosterDigest(persisted()) });
    });
    await settle();

    expect(names(persisted())).toContain("Queued");
    expect(names(main.hook().allPages)).toEqual(names(persisted()));
  });

  it("under StrictMode a save sends one broadcast and each window holds one listener", async () => {
    const main = await mountWindow("main", undefined, true);
    const pop = await mountWindow("term-2", undefined, true);
    expect(bus.listeners.get(TERMINAL_PAGES_SAVED_EVENT) ?? []).toHaveLength(2);

    act(() => void pop.hook().renamePage("p3", "Strict"));

    expect(savedEvents()).toHaveLength(1);
    expect(names(main.hook().allPages)).toEqual(["Alpha", "Beta", "Strict"]);
  });

  it("a foreign digest the store reaches after this window's own save is still applied", async () => {
    const main = await mountWindow("main");
    vi.useFakeTimers();
    const foreign = [ROSTER[0], { ...ROSTER[1], name: "Theirs" }, ROSTER[2]];
    // The foreign event arrives before its write is visible here: main retries.
    receiveFrom("term-2", rosterDigest(foreign));
    // Main saves meanwhile; its own echo must not cancel the pending read.
    act(() => void main.hook().renamePage("p3", "Mine"));
    // The other window's write turns out to be the store's last commit.
    instanceStorage.setJSON(PAGES_STORAGE_KEY, foreign);
    act(() => void vi.advanceTimersByTime(1_000));
    vi.useRealTimers();

    expect(names(main.hook().allPages)).toEqual(["Alpha", "Theirs", "Gamma"]);
  });

  it("an add that overtakes a pending transition write is applied once", async () => {
    // React renders the add first, skipping the transition write, then re-runs
    // the add after it -- on a base that already holds the add's own save.
    const main = await mountWindow("main");
    let added = "";
    act(() => {
      startTransition(() => {
        main.hook().reorderPage("p3", "p1");
      });
      flushSync(() => {
        added = main.hook().addPage("Once");
      });
    });
    await settle();

    expect(ids(persisted()).filter((id) => id === added)).toHaveLength(1);
    expect(ids(main.hook().allPages).filter((id) => id === added)).toHaveLength(1);
    expect(ids(persisted())[0]).toBe("p3");
  });

  it("this window's own save during a pending foreign read is not warned about", async () => {
    const main = await mountWindow("main");
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    vi.useFakeTimers();
    // A foreign digest the store never reaches: main starts retrying.
    receiveFrom("term-2", rosterDigest([ROSTER[0]]));
    // Main then saves: the store then holds its own save, so the exhausted
    // read is not warned about.
    act(() => void main.hook().renamePage("p2", "Own"));
    act(() => void vi.advanceTimersByTime(1_000));
    vi.useRealTimers();

    expect(warn).not.toHaveBeenCalled();
    expect(names(main.hook().allPages)).toEqual(["Alpha", "Own", "Gamma"]);
  });

  it("own echo: the saving window ignores its own event", async () => {
    const pop = await mountWindow("term-2");
    bus.hold = true;
    act(() => void pop.hook().renamePage("p2", "Mine"));
    expect(names(pop.hook().allPages)).toEqual(["Alpha", "Mine", "Gamma"]);
    const rendersAfterWrite = pop.renders();
    const echo = bus.held.splice(0);
    expect(echo).toHaveLength(1);
    expect((echo[0].payload as { source: string }).source).toBe("term-2");

    // The store moves on behind the sender's back with no event of its own;
    // a sender that read on its own echo would install it after the retries.
    instanceStorage.setJSON(PAGES_STORAGE_KEY, [ROSTER[0]]);
    vi.useFakeTimers();
    act(() => dispatch(echo[0].event, echo[0].payload));
    act(() => void vi.advanceTimersByTime(1_000));
    vi.useRealTimers();

    expect(names(pop.hook().allPages)).toEqual(["Alpha", "Mine", "Gamma"]);
    expect(pop.renders()).toBe(rendersAfterWrite);
  });

  it("a refused save broadcasts nothing, and the other window keeps its copy", async () => {
    const main = await mountWindow("main");
    const pop = await mountWindow("term-2");
    const before = main.hook().allPages;
    failWritesTo(PAGES_STORAGE_KEY);

    act(() => void pop.hook().renamePage("p2", "Unsaved"));

    expect(names(pop.hook().allPages)).toEqual(["Alpha", "Unsaved", "Gamma"]);
    expect(names(persisted())).toEqual(["Alpha", "Beta", "Gamma"]);
    expect(savedEvents()).toHaveLength(0);
    expect(main.hook().allPages).toBe(before);
  });

  it("a window whose last save was refused does not reload over its newer copy", async () => {
    const main = await mountWindow("main");
    const pop = await mountWindow("term-2");
    failWritesTo(PAGES_STORAGE_KEY);
    act(() => void main.hook().renamePage("p2", "Unsaved"));
    expect(names(persisted())).toEqual(["Alpha", "Beta", "Gamma"]);
    // Writes are accepted again; another window then saves and broadcasts.
    vi.mocked(Storage.prototype.setItem).mockRestore();

    act(() => void pop.hook().renamePage("p3", "Theirs"));

    expect(names(persisted())).toEqual(["Alpha", "Beta", "Theirs"]);
    expect(names(main.hook().allPages)).toEqual(["Alpha", "Unsaved", "Gamma"]);
  });

  it("unmounting clears a read still retrying", async () => {
    const main = await mountWindow("main");
    vi.useFakeTimers();
    receiveFrom("term-2", rosterDigest([ROSTER[0]]));
    expect(vi.getTimerCount()).toBeGreaterThan(0);
    main.unmount();
    mounted.splice(mounted.indexOf(main), 1);
    expect(vi.getTimerCount()).toBe(0);
    vi.useRealTimers();
  });

  it("a save made before this window's listener registered is caught up on", async () => {
    // Main mounts and reads the roster, but its roster listener is not yet
    // registered when another window saves, so no event reaches it.
    bus.deferEvent = TERMINAL_PAGES_SAVED_EVENT;
    const main = await mountWindow("main");
    expect(names(main.hook().allPages)).toEqual(["Alpha", "Beta", "Gamma"]);
    instanceStorage.setJSON(PAGES_STORAGE_KEY, [ROSTER[0], { ...ROSTER[1], name: "Early" }, ROSTER[2]]);

    bus.deferEvent = null;
    for (const release of bus.release.splice(0)) release();
    await settle();

    expect(names(main.hook().allPages)).toEqual(["Alpha", "Early", "Gamma"]);
  });

  it("a save built on an empty store's default page still matches the receiver's read at once", async () => {
    localStorage.clear();
    const main = await mountWindow("main");
    const pop = await mountWindow("term-2");
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});

    act(() => void pop.hook().addPage("New"));

    // Applied on the first read, with no retry and no lag warning.
    expect(names(main.hook().allPages)).toEqual(["Terminal", "New"]);
    expect(warn).not.toHaveBeenCalled();
  });

  it("an own save whose echo never arrives is still not warned about", async () => {
    const main = await mountWindow("main");
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    vi.useFakeTimers();
    receiveFrom("term-2", rosterDigest([ROSTER[0]]));
    // Main saves, but its own echo is held back past the retry budget.
    bus.hold = true;
    act(() => void main.hook().renamePage("p2", "EchoLate"));
    act(() => void vi.advanceTimersByTime(1_000));
    vi.useRealTimers();

    expect(warn).not.toHaveBeenCalled();
    expect(names(main.hook().allPages)).toEqual(["Alpha", "EchoLate", "Gamma"]);
  });

  it("a null store leaves the receiver's roster unchanged", async () => {
    const main = await mountWindow("main");
    const before = main.hook().allPages;
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    localStorage.clear();

    vi.useFakeTimers();
    receiveFrom("term-2", rosterDigest([ROSTER[0]]));
    act(() => void vi.advanceTimersByTime(1_000));
    vi.useRealTimers();

    expect(main.hook().allPages).toBe(before);
    expect(names(main.hook().allPages)).toEqual(["Alpha", "Beta", "Gamma"]);
    // Nothing was ever read, so there is nothing to apply or warn about.
    expect(warn).not.toHaveBeenCalled();
  });

  it("a newer foreign event supersedes a read still retrying on an older digest", async () => {
    const main = await mountWindow("main");
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    vi.useFakeTimers();
    // The older digest is never reached: main starts retrying on it.
    receiveFrom("term-2", rosterDigest([ROSTER[0]]));
    const newer = [ROSTER[0], { ...ROSTER[1], name: "Newer" }, ROSTER[2]];
    instanceStorage.setJSON(PAGES_STORAGE_KEY, newer);
    receiveFrom("term-3", rosterDigest(newer));
    act(() => void vi.advanceTimersByTime(1_000));
    expect(vi.getTimerCount()).toBe(0);
    vi.useRealTimers();

    expect(names(main.hook().allPages)).toEqual(["Alpha", "Newer", "Gamma"]);
    // The superseded read is gone, so it never exhausts into a warning.
    expect(warn).not.toHaveBeenCalled();
  });

  it("an own save that lands before a foreign event is not warned about either", async () => {
    const main = await mountWindow("main");
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    // Main saves first; the store holds its write.
    act(() => void main.hook().renamePage("p1", "MineFirst"));
    vi.useFakeTimers();
    // A foreign event whose write the store will never show (main overwrote it).
    receiveFrom("term-2", rosterDigest([ROSTER[2]]));
    act(() => void vi.advanceTimersByTime(1_000));
    vi.useRealTimers();

    expect(warn).not.toHaveBeenCalled();
    expect(names(main.hook().allPages)).toEqual(["MineFirst", "Beta", "Gamma"]);
  });

  it("a store emptied between the read and the update keeps the receiver's copy", async () => {
    const main = await mountWindow("main");
    const before = main.hook().allPages;
    const foreign = [ROSTER[0]];
    instanceStorage.setJSON(PAGES_STORAGE_KEY, foreign);

    act(() => {
      // The read matches at once and queues the reload...
      dispatch(TERMINAL_PAGES_SAVED_EVENT, { source: "term-2", digest: rosterDigest(foreign) });
      // ...but the store reads empty by the time the update runs.
      localStorage.clear();
    });

    expect(main.hook().allPages).toBe(before);
  });

  it("an unchanged roster keeps the same allPages reference", async () => {
    const main = await mountWindow("main");
    const before = main.hook().allPages;

    receiveFrom("term-2", rosterDigest(ROSTER));

    expect(main.hook().allPages).toBe(before);
  });

  it("the store lagging the event: the receiver still ends on the new roster", async () => {
    const main = await mountWindow("main");
    const pop = await mountWindow("term-2");

    bus.hold = true;
    act(() => void pop.hook().renamePage("p2", "Late"));
    const key = Object.keys(localStorage).find(
      (k) => k === PAGES_STORAGE_KEY || k.startsWith(`${PAGES_STORAGE_KEY}:`),
    )!;
    const written = localStorage.getItem(key)!;
    // The event reaches main while its view of the store still holds the old roster.
    localStorage.setItem(key, JSON.stringify(ROSTER));
    const [event] = bus.held.splice(0);

    vi.useFakeTimers();
    act(() => dispatch(event.event, event.payload));
    expect(names(main.hook().allPages)).toEqual(["Alpha", "Beta", "Gamma"]);
    // The write becomes visible before the first retry.
    localStorage.setItem(key, written);
    act(() => void vi.advanceTimersByTime(50));
    vi.useRealTimers();

    expect(names(main.hook().allPages)).toEqual(["Alpha", "Late", "Gamma"]);
  });
});
