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
  let props: { workMode: TerminalWorkMode; assignments: ZoneAssignments } = {
    workMode: "grid",
    assignments: {},
  };
  function Host() {
    harness.beginRender();
    useRecordedZoneBackstop({ ...props, tabs, pageId: "p", recordedZones: ledger });
  }
  return (next: typeof props) => {
    props = next;
    harness.renderSettled(Host);
    vi.advanceTimersByTime(ZONE_BACKSTOP_DEBOUNCE_MS);
  };
}

/** The `[sessionId, zoneIndex]` pairs the backstop recorded. */
function recorded(): Array<[string, number]> {
  return invoke.mock.calls.map((call) => {
    const args = call[1] as { claudeSessionId: string; zoneIndex: number };
    return [args.claudeSessionId, args.zoneIndex];
  });
}

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
});
