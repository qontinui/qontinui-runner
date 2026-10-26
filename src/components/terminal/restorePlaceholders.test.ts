/**
 * The restart cohort's placeholder rows and their app-wide store (plan
 * `2026-10-21-operator-prepare-for-restart-and-progressive-restore`, Phase 7).
 */

import { describe, it, expect, vi } from "vitest";

const mockInvoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
}));

import {
  RestoreCohortStore,
  bulkEligible,
  cohortCounts,
  cohortStripText,
  effortBadgeText,
  missingPlaceholderPages,
  onPlaceholderEvent,
  parsePlaceholderSet,
  placeholderSessionIdOf,
  restorePlaceholderTabId,
  setPlaceholderFinished,
  setPlaceholderHeld,
  RESTART_PREP_SET_HELD_COMMAND,
  placeholderMayTakeZone,
  COHORT_REFRESH_MS,
  COHORT_REFRESH_SWEEP_PENDING_MS,
  cohortRefreshInterval,
  stateAtPrepareText,
  toPlaceholder,
  type CohortRowWire,
  type PlaceholderRecord,
  type PlaceholderSet,
  type RestorePlaceholder,
} from "./restorePlaceholders";
import {
  placeholdersToCreate,
  readyZoneAssignments,
  stalePlaceholderSessions,
} from "./useRestorePlaceholderBinding";
import { reconcileTabsWithBackend, restorePlaceholderTab } from "./useTerminalManager";
import { pageRestoreModel } from "./RestorePageBar";
import { ensurePagesPresent } from "./useTerminalPages";

const rec = (sid: string, o: Partial<PlaceholderRecord> = {}): PlaceholderRecord => ({
  claudeSessionId: sid,
  pageId: "default",
  zoneIndex: 1,
  terminalId: `t-${sid}`,
  openedAt: 0,
  lastSeenAt: 0,
  state: "open",
  workingDir: "/w",
  configDir: "/cfg/a",
  restorePlaceholder: true,
  ...o,
});

const cohortRow = (sid: string, o: Partial<CohortRowWire> = {}): CohortRowWire => ({
  session: { claudeSessionId: sid, accountLabel: "acct-a", planSlug: "my-plan" },
  effort: { level: "max", source: "screen" },
  held: false,
  state_at_prepare: "idle",
  limit_reset_at: null,
  ...o,
});

const placeholder = (o: Partial<RestorePlaceholder> = {}): RestorePlaceholder =>
  toPlaceholder("prep-1", rec(o.claudeSessionId ?? "s"), cohortRow(o.claudeSessionId ?? "s"));

describe("parsePlaceholderSet", () => {
  it("reads placeholders only with a restoring prepare", () => {
    expect(parsePlaceholderSet({ sessions: [] })).toEqual({ placeholders: [], restartPrep: null });
    // Not a restore set at all: UNKNOWN, never "no cohort".
    expect(parsePlaceholderSet(undefined)).toBeNull();
    expect(parsePlaceholderSet([])).toBeNull();
    const out = parsePlaceholderSet({
      placeholders: [{ claudeSessionId: "a" }, { nope: 1 }],
      restartPrep: { prepareId: "p", holderSweepPending: true },
    });
    expect(out!.restartPrep).toEqual({ prepareId: "p", holderSweepPending: true });
    expect(out!.placeholders.map((p) => p.claudeSessionId)).toEqual(["a"]);
  });
});

describe("toPlaceholder", () => {
  it("joins the record with its cohort row", () => {
    const p = toPlaceholder("prep-1", rec("a", { sessionName: "named" }), cohortRow("a"));
    expect(p).toMatchObject({
      name: "named",
      accountLabel: "acct-a",
      planSlug: "my-plan",
      effortLevel: "max",
      held: false,
      finished: false,
      stateAtPrepare: "idle",
    });
  });

  it("never renders an unread effort or hold as a value", () => {
    const before = toPlaceholder("prep-1", rec("a"), undefined);
    expect(before.held).toBeNull();
    expect(before.effortLevel).toBeNull();
    expect(effortBadgeText(before)).toBe("effort unknown · hold not read yet");
    expect(bulkEligible(before)).toBe(false);
    const unread = toPlaceholder(
      "prep-1",
      rec("a"),
      cohortRow("a", { effort: { level: "max", source: "unknown" } }),
    );
    expect(unread.effortLevel).toBeNull();
  });

  it("names a usage-limit hit's reset time", () => {
    const p = toPlaceholder(
      "prep-1",
      rec("a"),
      cohortRow("a", { state_at_prepare: "limit_hit", limit_reset_at: 1_700_000_000_000 }),
    );
    expect(stateAtPrepareText(p)).toMatch(/^usage limit hit · account resets at /);
    const unknownReset = toPlaceholder(
      "prep-1",
      rec("a"),
      cohortRow("a", { state_at_prepare: "limit_hit" }),
    );
    expect(stateAtPrepareText(unknownReset)).toBe("usage limit hit · account reset time unknown");
  });

  it("says held for hand restore when held", () => {
    const p = toPlaceholder("prep-1", rec("a"), cohortRow("a", { held: true }));
    expect(effortBadgeText(p)).toBe("effort max · held for hand restore");
  });
});

describe("the strip", () => {
  it("renders nothing with no cohort, and keeps Dismiss reachable with zero rows", () => {
    expect(cohortStripText([], "2026-10-22T10:00:00Z", null)).toBeNull();
    expect(cohortStripText([], null, "prep-1")).toBe(
      "Restart cohort from an earlier prepare: nothing waiting to restore",
    );
  });
  it("counts one row", () => {
    expect(cohortStripText([placeholder()], null)).toBe(
      "Restart cohort from an earlier prepare: 1 waiting · 0 held",
    );
  });
  it("counts many rows by kind", () => {
    const rows = [
      placeholder({ claudeSessionId: "a" }),
      placeholder({ claudeSessionId: "b" }),
      { ...placeholder({ claudeSessionId: "c" }), held: true },
      { ...placeholder({ claudeSessionId: "d" }), held: null },
      { ...placeholder({ claudeSessionId: "e" }), finished: true },
      { ...placeholder({ claudeSessionId: "f" }), blocker: "boot_restore_owns_row" },
    ];
    expect(cohortCounts(rows)).toEqual({ waiting: 2, held: 2, finished: 1, blocked: 1 });
    expect(rows.filter(bulkEligible).map((r) => r.claudeSessionId)).toEqual(["a", "b"]);
    expect(cohortStripText(rows, null)).toBe(
      "Restart cohort from an earlier prepare: 2 waiting · 2 held · 1 blocked · 1 finished",
    );
  });
});

describe("RestoreCohortStore", () => {
  const status = (rows: CohortRowWire[]) => ({
    readStatus: vi.fn(async () => ({ startedAt: "2026-10-22T10:00:00Z", rows })),
    readSet: vi.fn(async () => null),
  });

  it("joins the restore set with the prepare's status", async () => {
    const io = status([cohortRow("a"), cohortRow("b", { held: true })]);
    const store = new RestoreCohortStore(io);
    store.publish({
      placeholders: [rec("b", { pageId: "p2" }), rec("a", { pageId: "p1" })],
      restartPrep: { prepareId: "prep-1", holderSweepPending: false },
    });
    expect(store.getSnapshot().statusLoaded).toBe(false);
    await store.refreshStatus();
    const snap = store.getSnapshot();
    expect(snap.statusLoaded).toBe(true);
    expect(snap.startedAt).toBe("2026-10-22T10:00:00Z");
    expect(snap.rows.map((r) => [r.claudeSessionId, r.held])).toEqual([
      ["a", false],
      ["b", true],
    ]);
  });

  it("Finish on a placeholder removes it from bulk; un-finish puts it back", async () => {
    const store = new RestoreCohortStore(status([cohortRow("a")]));
    store.publish({
      placeholders: [rec("a")],
      restartPrep: { prepareId: "prep-1", holderSweepPending: false },
    });
    await store.refreshStatus();
    expect(bulkEligible(store.row("a")!)).toBe(true);
    mockInvoke.mockResolvedValueOnce({ success: true, data: {} });
    expect(await setPlaceholderFinished("a", true, store)).toBeNull();
    expect(mockInvoke).toHaveBeenLastCalledWith(
      "terminal_session_set_finished",
      expect.objectContaining({ claudeSessionId: "a", finished: true }),
    );
    expect(store.row("a")!.finished).toBe(true);
    expect(bulkEligible(store.row("a")!)).toBe(false);
    mockInvoke.mockResolvedValueOnce({ success: true, data: {} });
    expect(await setPlaceholderFinished("a", false, store)).toBeNull();
    expect(bulkEligible(store.row("a")!)).toBe(true);
  });

  it("a refused finish changes nothing", async () => {
    const store = new RestoreCohortStore(status([cohortRow("a")]));
    store.publish({
      placeholders: [rec("a")],
      restartPrep: { prepareId: "prep-1", holderSweepPending: false },
    });
    mockInvoke.mockResolvedValueOnce({ success: false, message: "no such session", data: null });
    expect(await setPlaceholderFinished("a", true, store)).toBe("no such session");
    expect(store.row("a")!.finished).toBe(false);
  });

  it("the held toggle goes through the one command name and only lands once accepted", async () => {
    const store = new RestoreCohortStore(status([cohortRow("a")]));
    store.publish({
      placeholders: [rec("a")],
      restartPrep: { prepareId: "prep-1", holderSweepPending: false },
    });
    await store.refreshStatus();
    mockInvoke.mockResolvedValueOnce(undefined);
    expect(await setPlaceholderHeld("prep-1", "a", true, store)).toBeNull();
    expect(mockInvoke).toHaveBeenLastCalledWith(RESTART_PREP_SET_HELD_COMMAND, {
      prepareId: "prep-1",
      claudeSessionId: "a",
      held: true,
    });
    expect(store.row("a")!.held).toBe(true);
    mockInvoke.mockRejectedValueOnce("unknown command");
    expect(await setPlaceholderHeld("prep-1", "a", false, store)).toBe("unknown command");
    expect(store.row("a")!.held).toBe(true);
  });

  it("a re-read drops a session that came back by another path; an unreadable one changes nothing", async () => {
    const reads: Array<PlaceholderSet | null> = [];
    const store = new RestoreCohortStore({
      readStatus: async () => null,
      readSet: async () => reads.shift() ?? null,
    });
    store.publish({
      placeholders: [rec("a"), rec("b")],
      restartPrep: { prepareId: "prep-1", holderSweepPending: true },
    });
    expect(cohortRefreshInterval(store.getSnapshot())).toBe(COHORT_REFRESH_SWEEP_PENDING_MS);
    // The holder census came in: `a` was adopted, so it is no placeholder.
    reads.push({
      placeholders: [rec("b")],
      restartPrep: { prepareId: "prep-1", holderSweepPending: false },
    });
    await store.refreshSet();
    expect(store.getSnapshot().rows.map((r) => r.claudeSessionId)).toEqual(["b"]);
    expect(cohortRefreshInterval(store.getSnapshot())).toBe(COHORT_REFRESH_MS);
    // A read that failed is UNKNOWN: the placeholders stay.
    await store.refreshSet();
    expect(store.getSnapshot().rows.map((r) => r.claudeSessionId)).toEqual(["b"]);
    // A restore landed while a read was in flight: that read is dropped.
    let release: (v: PlaceholderSet | null) => void = () => {};
    const slow = new Promise<PlaceholderSet | null>((r) => (release = r));
    const store2 = new RestoreCohortStore({ readStatus: async () => null, readSet: () => slow });
    store2.publish({
      placeholders: [rec("x")],
      restartPrep: { prepareId: "prep-1", holderSweepPending: false },
    });
    const inFlight = store2.refreshSet();
    store2.markBack("x");
    release({
      placeholders: [rec("x")],
      restartPrep: { prepareId: "prep-1", holderSweepPending: false },
    });
    await inFlight;
    expect(store2.getSnapshot().rows).toEqual([]);
    // The backend finished the cohort: nothing is restoring any more.
    reads.push({ placeholders: [], restartPrep: null });
    await store.refreshSet();
    expect(store.getSnapshot().prepareId).toBeNull();
    expect(cohortRefreshInterval(store.getSnapshot())).toBeNull();
  });

  it("an unreadable first read keeps re-reading until one lands", async () => {
    const reads: Array<PlaceholderSet | null> = [];
    const store = new RestoreCohortStore({
      readStatus: async () => null,
      readSet: async () => reads.shift() ?? null,
    });
    expect(cohortRefreshInterval(store.getSnapshot())).toBeNull();
    store.markUnread();
    expect(cohortRefreshInterval(store.getSnapshot())).toBe(COHORT_REFRESH_MS);
    await store.refreshSet();
    expect(store.getSnapshot().unread).toBe(true);
    reads.push({
      placeholders: [rec("a")],
      restartPrep: { prepareId: "prep-1", holderSweepPending: false },
    });
    await store.refreshSet();
    expect(store.getSnapshot()).toMatchObject({ unread: false, prepareId: "prep-1" });
    expect(store.getSnapshot().rows.map((r) => r.claudeSessionId)).toEqual(["a"]);
  });

  // Finding 1 (review): the store read "no cohort" at boot (before the record
  // reached `restoring`). Release hold then moves the record to `restoring`
  // and announces it; the store must read the set although it holds no
  // cohort, or no placeholder and no strip ever appear.
  it("a restart-prep-changed naming a restoring cohort is read from the no-cohort state", async () => {
    const reads: Array<PlaceholderSet | null> = [];
    const io = {
      readStatus: vi.fn(async () => ({ startedAt: null, rows: [cohortRow("a")] })),
      readSet: vi.fn(async () => reads.shift() ?? null),
    };
    const store = new RestoreCohortStore(io);
    store.publish({ placeholders: [], restartPrep: null });
    expect(cohortRefreshInterval(store.getSnapshot())).toBeNull();
    // A notice tick during a prepare announces nothing restoring: no read.
    await store.onPrepChanged({ record: { prepare_id: "prep-1", state: "preparing" } });
    expect(io.readSet).not.toHaveBeenCalled();
    reads.push({
      placeholders: [rec("a")],
      restartPrep: { prepareId: "prep-1", holderSweepPending: false },
    });
    await store.onPrepChanged({ record: { prepare_id: "prep-1", state: "restoring" } });
    expect(io.readSet).toHaveBeenCalledTimes(1);
    expect(store.getSnapshot().prepareId).toBe("prep-1");
    expect(store.getSnapshot().rows.map((r) => r.claudeSessionId)).toEqual(["a"]);
  });

  // Finding 4 (review): the cohort leaving `restoring` (dismissed from any
  // surface) ends it here at once, placeholder tabs included.
  it("a restart-prep-changed that ends the held cohort empties the store", async () => {
    const store = new RestoreCohortStore(status([]));
    store.publish({
      placeholders: [rec("a")],
      restartPrep: { prepareId: "prep-1", holderSweepPending: false },
    });
    const events: string[] = [];
    const off = onPlaceholderEvent((e) => events.push(e.kind));
    await store.onPrepChanged({ record: { prepare_id: "prep-other", state: "done" } });
    expect(store.getSnapshot().prepareId).toBe("prep-1");
    await store.onPrepChanged({ record: { prepare_id: "prep-1", state: "done" } });
    off();
    expect(store.getSnapshot()).toMatchObject({ prepareId: null, rows: [] });
    expect(events).toEqual(["dismissed"]);
  });

  it("a read with no restoring cohort, a restore, or a dismissal empties it", () => {
    const store = new RestoreCohortStore(status([]));
    store.publish({
      placeholders: [rec("a"), rec("b")],
      restartPrep: { prepareId: "prep-1", holderSweepPending: false },
    });
    store.markBack("a");
    expect(store.getSnapshot().rows.map((r) => r.claudeSessionId)).toEqual(["b"]);
    store.publish({ placeholders: [], restartPrep: null });
    expect(store.getSnapshot()).toMatchObject({ prepareId: null, rows: [] });
    store.publish({
      placeholders: [rec("a")],
      restartPrep: { prepareId: "prep-2", holderSweepPending: false },
    });
    store.clear();
    expect(store.getSnapshot().rows).toEqual([]);
  });
});

describe("page helpers", () => {
  it("names the pages to recreate once each", () => {
    expect(
      missingPlaceholderPages([{ pageId: "x" }, { pageId: "x" }, { pageId: "default" }], []),
    ).toEqual([
      { id: "x", name: "Restored x" },
      { id: "default", name: "Terminal" },
    ]);
  });

  it("recreates a missing page under its own id, and nothing else", () => {
    const pages = [{ id: "default", name: "Terminal", createdAt: 0 }];
    expect(ensurePagesPresent(pages, [{ id: "default", name: "x" }])).toBe(pages);
    const next = ensurePagesPresent(pages, [{ id: "gone", name: "Restored gone" }]);
    expect(next.map((p) => [p.id, p.name])).toEqual([
      ["default", "Terminal"],
      ["gone", "Restored gone"],
    ]);
  });

  it("counts a page's bulk-eligible placeholders", () => {
    const rows = [
      { ...placeholder({ claudeSessionId: "a" }), pageId: "p" },
      { ...placeholder({ claudeSessionId: "b" }), pageId: "p", held: true },
      { ...placeholder({ claudeSessionId: "c" }), pageId: "q" },
    ];
    expect(pageRestoreModel(rows, "p")?.eligible).toBe(1);
    expect(pageRestoreModel(rows, "p")?.rows).toHaveLength(2);
    expect(pageRestoreModel(rows, "none")).toBeNull();
  });
});

describe("placeholder tabs", () => {
  it("are keyed by session and carry no running session id", () => {
    const tab = restorePlaceholderTab("abc", "name", "/w", 1);
    expect(tab.id).toBe(restorePlaceholderTabId("abc"));
    expect(placeholderSessionIdOf(tab.id)).toBe("abc");
    expect(placeholderSessionIdOf("t-1")).toBeNull();
    expect(tab.claudeSessionId).toBeUndefined();
    expect(tab.type).toBe("restore-placeholder");
  });

  it("survive the backend re-sync, which never lists them", () => {
    const tab = restorePlaceholderTab("abc", "name", null, 0);
    expect(reconcileTabsWithBackend([tab], [], 1_000_000)).toEqual([tab]);
  });

  it("go once their session is no longer waiting", () => {
    const tabs = [
      restorePlaceholderTab("a", "a", null),
      restorePlaceholderTab("b", "b", null),
      { ...restorePlaceholderTab("x", "x", null), id: "t-live", type: undefined },
    ];
    expect(stalePlaceholderSessions(tabs, [{ claudeSessionId: "a" }])).toEqual(["b"]);
  });

  it("are added for sessions newly waiting on this page, once, and never over a live one", () => {
    const rows = [
      { ...placeholder({ claudeSessionId: "a" }), pageId: "p" },
      { ...placeholder({ claudeSessionId: "b" }), pageId: "p" },
      { ...placeholder({ claudeSessionId: "c" }), pageId: "q" },
      { ...placeholder({ claudeSessionId: "d" }), pageId: "p" },
    ];
    const out = placeholdersToCreate(rows, "p", new Set(["b"]), [{ claudeSessionId: "d" }]);
    expect(out.map((r) => r.claudeSessionId)).toEqual(["a"]);
  });

  it("take their recorded zone only when it is free", () => {
    expect(placeholderMayTakeZone(2, "ph", {})).toBe(true);
    expect(placeholderMayTakeZone(2, "ph", { 2: "ph" })).toBe(true);
    expect(placeholderMayTakeZone(2, "ph", { 2: "t-live" })).toBe(false);
    expect(placeholderMayTakeZone(-1, "ph", {})).toBe(false);
  });

  it("hand the zone to the live pane once its tab has arrived", () => {
    const pending = new Map([
      ["t-new", 3],
      ["t-later", 1],
    ]);
    expect(readyZoneAssignments(pending, [{ id: "t-new" }])).toEqual([["t-new", 3]]);
  });
});
