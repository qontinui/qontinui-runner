/**
 * The zone re-resolution backstop stands down in tmux work mode (plan
 * `2026-10-11-runner-terminal-tmux-work-mode`): re-resolving against the one
 * viewport would rewrite every off-screen session's durable record to `-1`
 * and the shown one's to `0`, so a restore after a runner restart — and the
 * exit from tmux — would re-deal the grid instead of restoring it.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("react", async () => {
  const { hooksHarness } = await import("@/lib/__test-helpers__/hooks-harness");
  return hooksHarness.reactMock;
});
const invoke = vi.hoisted(() => vi.fn((..._args: unknown[]) => Promise.resolve(null)));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { hooksHarness as harness } from "@/lib/__test-helpers__/hooks-harness";
import { ZONE_BACKSTOP_DEBOUNCE_MS, useRecordedZoneBackstop } from "./useRecordedZoneBackstop";
import type { RecordedZoneLedger } from "./sessionRecordArgs";
import type { TerminalWorkMode } from "./terminalWorkMode";
import type { ZoneAssignments } from "./useZoneLayout";

const tabs = [
  { id: "a", title: "a", claudeSessionId: "sess-a" },
  { id: "b", title: "b", claudeSessionId: "sess-b" },
];

function mount(ledger: RecordedZoneLedger) {
  harness.reset();
  let props: {
    workMode: TerminalWorkMode;
    assignments: ZoneAssignments;
    restoreComplete?: boolean;
  } = {
    workMode: "grid",
    assignments: {},
  };
  function Host() {
    harness.beginRender();
    useRecordedZoneBackstop({
      restoreComplete: true,
      ...props,
      tabs,
      pageId: "p",
      recordedZones: ledger,
    });
  }
  return (next: typeof props) => {
    props = next;
    harness.renderSettled(Host);
    vi.advanceTimersByTime(ZONE_BACKSTOP_DEBOUNCE_MS);
  };
}

/** The `[sessionId, zoneIndex]` pairs the backstop sent to `command`. */
function sentTo(command: string): Array<[string, number]> {
  return invoke.mock.calls
    .filter((call) => call[0] === command)
    .map((call) => {
      const args = call[1] as { claudeSessionId: string; zoneIndex: number };
      return [args.claudeSessionId, args.zoneIndex];
    });
}

/** The `[sessionId, zoneIndex]` pairs the backstop re-recorded with `record_open`. */
const recorded = () => sentTo("terminal_session_record_open");
/** The `[sessionId, zoneIndex]` pairs the backstop zone-synced. */
const synced = () => sentTo("terminal_session_sync_zone");

beforeEach(() => {
  vi.useFakeTimers();
  invoke.mockClear();
});
afterEach(() => vi.useRealTimers());

describe("useRecordedZoneBackstop", () => {
  it("grid mode: a session whose zone moved is re-recorded at its new zone", () => {
    const ledger: RecordedZoneLedger = new Map([
      ["sess-a", 0],
      ["sess-b", 3],
    ]);
    mount(ledger)({ workMode: "grid", assignments: { 0: "a", 1: "b" } });
    expect(recorded()).toEqual([["sess-b", 1]]);
  });

  it("tmux mode: nothing is re-recorded, whatever the one viewport holds", () => {
    const ledger: RecordedZoneLedger = new Map([
      ["sess-a", 0],
      ["sess-b", 3],
    ]);
    const render = mount(ledger);
    // b is shown: zone 0 of the viewport, a in no zone at all.
    render({ workMode: "tmux", assignments: { 0: "b" } });
    render({ workMode: "tmux", assignments: { 0: "a" } });
    expect(recorded()).toEqual([]);
    expect(ledger.get("sess-b")).toBe(3);
  });

  it("back in grid mode, a session whose grid zone changed meanwhile is re-recorded", () => {
    const ledger: RecordedZoneLedger = new Map([
      ["sess-a", 0],
      ["sess-b", 3],
    ]);
    const render = mount(ledger);
    render({ workMode: "tmux", assignments: { 0: "b" } });
    render({ workMode: "grid", assignments: { 0: "a", 2: "b" } });
    expect(recorded()).toEqual([["sess-b", 2]]);
  });

  it("grid mode: a session this page never wrote is zone-synced, never re-recorded", () => {
    // Empty ledger: both records were written by the backend (a
    // `POST /terminals` session is recorded at zone 0), not by this page.
    const ledger: RecordedZoneLedger = new Map();
    mount(ledger)({ workMode: "grid", assignments: { 1: "a", 2: "b" } });
    expect(recorded()).toEqual([]);
    expect(synced()).toEqual([
      ["sess-a", 1],
      ["sess-b", 2],
    ]);
    // Keyed by the tab id — the id the backend stored as the record's terminal.
    expect(invoke.mock.calls.map((call) => call[1])).toEqual([
      { claudeSessionId: "sess-a", terminalId: "a", zoneIndex: 1 },
      { claudeSessionId: "sess-b", terminalId: "b", zoneIndex: 2 },
    ]);
  });

  it("grid mode: a first-seen session is synced once, not on every pass", () => {
    const ledger: RecordedZoneLedger = new Map();
    const render = mount(ledger);
    render({ workMode: "grid", assignments: { 1: "a", 2: "b" } });
    render({ workMode: "grid", assignments: { 1: "a", 2: "b" } });
    expect(synced()).toEqual([
      ["sess-a", 1],
      ["sess-b", 2],
    ]);
  });

  it("tmux mode: nothing is zone-synced either, and the ledger is not seeded", () => {
    const ledger: RecordedZoneLedger = new Map();
    const render = mount(ledger);
    render({ workMode: "tmux", assignments: { 0: "b" } });
    expect(invoke).not.toHaveBeenCalled();
    expect(ledger.size).toBe(0);
    // Back in grid mode the never-written sessions are first-seen there.
    render({ workMode: "grid", assignments: { 0: "a", 3: "b" } });
    expect(synced()).toEqual([
      ["sess-a", 0],
      ["sess-b", 3],
    ]);
    expect(recorded()).toEqual([]);
  });

  it("before the page's restore finishes, a first-seen session is neither synced nor seeded", () => {
    // A webview reload: reconnected tabs sit in localStorage / auto-fill zones
    // while the durable record is still what restore reads.
    const ledger: RecordedZoneLedger = new Map();
    const render = mount(ledger);
    render({ workMode: "grid", assignments: { 1: "a", 2: "b" }, restoreComplete: false });
    expect(invoke).not.toHaveBeenCalled();
    expect(ledger.size).toBe(0);
    // Restore done: the same sessions are first-seen now, at their live zones.
    render({ workMode: "grid", assignments: { 0: "a", 2: "b" }, restoreComplete: true });
    expect(synced()).toEqual([
      ["sess-a", 0],
      ["sess-b", 2],
    ]);
  });

  it("before the page's restore finishes, a session this page wrote still follows its zone", () => {
    const ledger: RecordedZoneLedger = new Map([["sess-a", 0]]);
    mount(ledger)({ workMode: "grid", assignments: { 3: "a", 1: "b" }, restoreComplete: false });
    expect(recorded()).toEqual([["sess-a", 3]]);
    expect(synced()).toEqual([]);
    expect(ledger.has("sess-b")).toBe(false);
  });
});
