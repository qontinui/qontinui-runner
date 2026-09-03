/**
 * Tests for the durable session-registry record payload builders.
 *
 * These cover the two frontend record contracts that the backend must satisfy:
 *   - OPEN: `terminal_session_record_open` resolves the tab's zone by
 *     reverse-lookup over the live assignments (the `onSessionIdBound` path).
 *   - CLOSE: `terminal_session_record_close` fires only for tabs that carry a
 *     `claudeSessionId` (explicit-close path in `useTerminalManager`).
 *
 * vitest runs `environment: "node"` (no React Testing Library), so we exercise
 * the extracted pure builders + a mocked `invoke` to assert the wire contract.
 */

import { describe, it, expect, vi } from "vitest";
import {
  buildSessionOpenArgs,
  describeRecordOpenOutcome,
  noteRecordedZone,
  planZoneReemits,
  readRecordOpenReport,
  recordedZoneLedgerFor,
  resetRecordedZoneLedgers,
  resolveZoneIndex,
  UNZONED_INDEX,
  type RecordedZoneLedger,
} from "./sessionRecordArgs";
import { buildSessionCloseRecord, type TerminalTab } from "./useTerminalManager";

const mockInvoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
}));

const tab = (id: string, overrides: Partial<TerminalTab> = {}): TerminalTab => ({
  id,
  title: id,
  pid: 1,
  isAlive: true,
  exitCode: null,
  ...overrides,
});

describe("resolveZoneIndex", () => {
  it("reverse-looks-up the zone a tab is assigned to", () => {
    expect(resolveZoneIndex({ 0: "shell", 3: "claudeB" }, "claudeB")).toBe(3);
  });
  it("returns -1 for a tab in no zone", () => {
    expect(resolveZoneIndex({ 0: "shell" }, "ghost")).toBe(-1);
  });
});

describe("buildSessionOpenArgs — onSessionIdBound zone resolution", () => {
  it("records the OPEN with the resolved zoneIndex + tab metadata", () => {
    const args = buildSessionOpenArgs({
      assignments: { 0: "shell-1", 3: "tab-claude" },
      tabs: [
        { id: "shell-1", title: "Terminal 1" },
        { id: "tab-claude", title: "claude", workingDir: "D:/repo" },
      ],
      tabId: "tab-claude",
      claudeSessionId: "sid-123",
      configDir: "C:/claude/.claude-hotmail",
      pageId: "default",
    });
    expect(args).toEqual({
      claudeSessionId: "sid-123",
      configDir: "C:/claude/.claude-hotmail",
      workingDir: "D:/repo",
      pageId: "default",
      zoneIndex: 3,
      title: "claude",
      terminalId: "tab-claude",
    });
  });

  // Omitted origin → key ABSENT so the backend preserves any existing origin.
  it("includes origin only when the caller asserts one", () => {
    const base = {
      assignments: {},
      tabs: [],
      tabId: "t",
      claudeSessionId: "sid",
      configDir: undefined,
      pageId: "default",
    };
    expect("origin" in buildSessionOpenArgs(base)).toBe(false);
    expect(buildSessionOpenArgs({ ...base, origin: "authoritative" }).origin).toBe("authoritative");
  });

  it("records zoneIndex -1 when the bound tab is unassigned", () => {
    const args = buildSessionOpenArgs({
      assignments: { 0: "other" },
      tabs: [{ id: "tab-claude", title: "claude" }],
      tabId: "tab-claude",
      claudeSessionId: "sid-xyz",
      configDir: undefined,
      pageId: "default",
    });
    expect(args.zoneIndex).toBe(-1);
    expect(args.terminalId).toBe("tab-claude");
  });

  it("fires terminal_session_record_open with the resolved args (mock invoke)", async () => {
    mockInvoke.mockReset();
    mockInvoke.mockResolvedValueOnce({ success: true, message: null, data: null });
    const { invoke } = await import("@tauri-apps/api/core");
    const args = buildSessionOpenArgs({
      assignments: { 2: "tab-claude" },
      tabs: [{ id: "tab-claude", title: "claude", workingDir: "D:/w" }],
      tabId: "tab-claude",
      claudeSessionId: "sid-1",
      configDir: "C:/cfg",
      pageId: "default",
    });
    await invoke("terminal_session_record_open", { ...args });
    expect(mockInvoke).toHaveBeenCalledWith("terminal_session_record_open", {
      claudeSessionId: "sid-1",
      configDir: "C:/cfg",
      workingDir: "D:/w",
      pageId: "default",
      zoneIndex: 2,
      title: "claude",
      terminalId: "tab-claude",
    });
  });
});

describe("buildSessionCloseRecord — close recording carries BOTH halves of the key", () => {
  it("returns explicit-close args for a tab carrying a claudeSessionId", () => {
    const tabs = [tab("shell"), tab("ai", { claudeSessionId: "sid-close" })];
    expect(buildSessionCloseRecord(tabs, "ai")).toEqual({
      claudeSessionId: "sid-close",
      terminalId: "ai",
      reason: "explicit",
    });
  });

  it("returns null for a plain shell (no claudeSessionId — nothing to record)", () => {
    const tabs = [tab("shell"), tab("ai", { claudeSessionId: "sid" })];
    expect(buildSessionCloseRecord(tabs, "shell")).toBeNull();
  });

  it("returns null when the tab id is unknown", () => {
    expect(buildSessionCloseRecord([tab("a")], "missing")).toBeNull();
  });

  /**
   * THE PAIR-PINNING ASSERTION. A tab can legitimately carry a
   * `claudeSessionId` that keys ANOTHER terminal's durable record — a
   * provisional spawn-seam id, a restored id whose pty was respawned under a
   * fresh `--session-id`, or a `reconciled` freshest-mtime bind that "may be
   * foreign". The payload must still name THIS tab's own terminal, so the
   * backend can detect the mis-binding and close the record this terminal
   * actually owns instead of the foreign one.
   */
  it("yields its OWN terminalId even when its claudeSessionId belongs to another tab", () => {
    const tabs = [
      tab("tab-other", { claudeSessionId: "sid-shared" }),
      tab("tab-stale", { claudeSessionId: "sid-shared" }),
    ];
    expect(buildSessionCloseRecord(tabs, "tab-stale")).toEqual({
      claudeSessionId: "sid-shared",
      terminalId: "tab-stale",
      reason: "explicit",
    });
  });

  it("carries the pty-exit reason and the exiting terminal's own id", () => {
    const tabs = [tab("shell"), tab("ai", { claudeSessionId: "sid-foreign" })];
    expect(buildSessionCloseRecord(tabs, "ai", "pty-exit")).toEqual({
      claudeSessionId: "sid-foreign",
      terminalId: "ai",
      reason: "pty-exit",
    });
  });

  it("fires terminal_session_record_close with both ids and reason 'explicit' (mock invoke)", async () => {
    mockInvoke.mockReset();
    mockInvoke.mockResolvedValueOnce({ success: true, message: null, data: null });
    const { invoke } = await import("@tauri-apps/api/core");
    const record = buildSessionCloseRecord([tab("ai", { claudeSessionId: "sid-close" })], "ai");
    expect(record).not.toBeNull();
    await invoke("terminal_session_record_close", record!);
    expect(mockInvoke).toHaveBeenCalledWith("terminal_session_record_close", {
      claudeSessionId: "sid-close",
      terminalId: "ai",
      reason: "explicit",
    });
  });
});

/**
 * Zone RE-RESOLUTION (iteration-4 item 2).
 *
 * `resolveZoneIndex` runs once, when a tab binds its `claudeSessionId` — which
 * is normally BEFORE `reconcileAssignments` has auto-filled that tab into a
 * zone, so the durable record is written `UNZONED_INDEX`. The backstop that is
 * supposed to fix that used to seed itself from the first zone it OBSERVED;
 * by then the tab was already in its final zone, so nothing looked like a
 * change and the `-1` stood for the life of the record. These tests pin the
 * ledger-of-WRITES semantics that makes the disagreement visible, and pin that
 * `-1` remains a legitimate recorded value rather than being clamped away.
 */
describe("planZoneReemits — durable zoneIndex re-resolution", () => {
  const obs = (claudeSessionId: string, tabId: string, zoneIndex: number) => ({
    claudeSessionId,
    tabId,
    zoneIndex,
  });

  it("re-resolves a record written -1 once the tab lands in a real zone", () => {
    const ledger: RecordedZoneLedger = new Map();
    // The id-bind path wrote the OPEN before the tab had a zone.
    noteRecordedZone(ledger, "sid-a", UNZONED_INDEX);

    const emits = planZoneReemits(ledger, [obs("sid-a", "tab-a", 2)]);

    expect(emits).toEqual([obs("sid-a", "tab-a", 2)]);
    expect(ledger.get("sid-a")).toBe(2);
  });

  it("is idempotent — a second pass over the same layout emits nothing", () => {
    const ledger: RecordedZoneLedger = new Map();
    noteRecordedZone(ledger, "sid-a", UNZONED_INDEX);
    planZoneReemits(ledger, [obs("sid-a", "tab-a", 2)]);

    expect(planZoneReemits(ledger, [obs("sid-a", "tab-a", 2)])).toEqual([]);
  });

  it("emits nothing when the written zone already matches the live zone", () => {
    const ledger: RecordedZoneLedger = new Map();
    noteRecordedZone(ledger, "sid-a", 3);

    expect(planZoneReemits(ledger, [obs("sid-a", "tab-a", 3)])).toEqual([]);
  });

  it("follows an operator move in BOTH directions, including back to unzoned", () => {
    const ledger: RecordedZoneLedger = new Map();
    noteRecordedZone(ledger, "sid-a", 0);

    expect(planZoneReemits(ledger, [obs("sid-a", "tab-a", 4)])).toEqual([obs("sid-a", "tab-a", 4)]);
    // Dragged out of every zone: `-1` is a real recorded value, so the record
    // follows the tab DOWN as well as up. It is never clamped to zone 0.
    expect(planZoneReemits(ledger, [obs("sid-a", "tab-a", UNZONED_INDEX)])).toEqual([
      obs("sid-a", "tab-a", UNZONED_INDEX),
    ]);
    expect(ledger.get("sid-a")).toBe(UNZONED_INDEX);
  });

  it("keeps -1 for a tab that legitimately stays unzoned past the zone ceiling", () => {
    const ledger: RecordedZoneLedger = new Map();
    noteRecordedZone(ledger, "sid-hidden", UNZONED_INDEX);

    // The tab is live but beyond the 9-zone ceiling — it resolves to no zone,
    // agrees with the record, and must produce no write at all.
    expect(planZoneReemits(ledger, [obs("sid-hidden", "tab-h", UNZONED_INDEX)])).toEqual([]);
    expect(ledger.get("sid-hidden")).toBe(UNZONED_INDEX);
  });

  it("seeds silently for a record this page never wrote (restore-owned)", () => {
    // No `noteRecordedZone` — the boot-restore path owns this row. Re-asserting
    // `record_open` for it would refresh `last_seen_at` on a row the restore
    // deliberately left alone, which is how ghost records became immortal.
    const ledger: RecordedZoneLedger = new Map();

    expect(planZoneReemits(ledger, [obs("sid-restored", "tab-r", 1)])).toEqual([]);
    expect(ledger.get("sid-restored")).toBe(1);
    // ...and once seeded it behaves normally on a later genuine move.
    expect(planZoneReemits(ledger, [obs("sid-restored", "tab-r", 5)])).toEqual([
      obs("sid-restored", "tab-r", 5),
    ]);
  });

  it("prunes sessions that no longer exist so a reused id re-seeds cleanly", () => {
    const ledger: RecordedZoneLedger = new Map();
    noteRecordedZone(ledger, "sid-gone", 0);

    planZoneReemits(ledger, []);
    expect(ledger.has("sid-gone")).toBe(false);

    // Re-seeded from scratch → treated as never-written, so no emit.
    expect(planZoneReemits(ledger, [obs("sid-gone", "tab-g", 7)])).toEqual([]);
  });

  it("handles several sessions in one pass, emitting only the disagreements", () => {
    const ledger: RecordedZoneLedger = new Map();
    noteRecordedZone(ledger, "sid-a", UNZONED_INDEX);
    noteRecordedZone(ledger, "sid-b", 1);

    expect(planZoneReemits(ledger, [obs("sid-a", "tab-a", 0), obs("sid-b", "tab-b", 1)])).toEqual([
      obs("sid-a", "tab-a", 0),
    ]);
  });
});

describe("recordedZoneLedgerFor — per-page ledgers", () => {
  it("returns a stable ledger per pageId and isolates pages from each other", () => {
    resetRecordedZoneLedgers();
    const a = recordedZoneLedgerFor("page-a");
    expect(recordedZoneLedgerFor("page-a")).toBe(a);

    noteRecordedZone(a, "sid-a", UNZONED_INDEX);
    const b = recordedZoneLedgerFor("page-b");
    expect(b).not.toBe(a);

    // Page B's prune must not evict page A's entry.
    planZoneReemits(b, []);
    expect(a.has("sid-a")).toBe(true);
    resetRecordedZoneLedgers();
  });
});

/**
 * `terminal_session_record_open` reports WRITTEN vs BOUND. Nothing on the
 * frontend read that report until `describeRecordOpenOutcome` — every writer
 * kept only a `.catch`, so a session that recorded and never confirmed was
 * indistinguishable from one that reached the tab.
 */
describe("describeRecordOpenOutcome (written vs bound)", () => {
  const bound = {
    success: true,
    data: { recorded: true, confirmed: true, confirmBy: "POST /control/session-open" },
  };
  const provisional = {
    success: true,
    data: { recorded: true, confirmed: false, confirmBy: "POST /control/session-open" },
  };

  it("reads the report out of the command response", () => {
    expect(readRecordOpenReport(bound)).toEqual({
      recorded: true,
      confirmed: true,
      confirmBy: "POST /control/session-open",
    });
    expect(readRecordOpenReport(provisional)?.confirmed).toBe(false);
  });

  it("says BOUND for a confirmed row — terminal_list will surface it", () => {
    const line = describeRecordOpenOutcome({
      claudeSessionId: "sess-1",
      terminalId: "terminal-live-7",
      response: bound,
    });
    expect(line).toContain("BOUND");
    expect(line).toContain("sess-1");
    expect(line).toContain("terminal-live-7");
    expect(line).not.toContain("PROVISIONAL");
  });

  it("says PROVISIONAL for an unconfirmed row and names the door", () => {
    const line = describeRecordOpenOutcome({
      claudeSessionId: "sess-2",
      terminalId: "terminal-live-8",
      response: provisional,
    });
    expect(line).toContain("PROVISIONAL");
    expect(line).toContain("POST /control/session-open");
  });

  it("falls back to the canonical door when the payload omits confirmBy", () => {
    const line = describeRecordOpenOutcome({
      claudeSessionId: "sess-3",
      terminalId: "t-3",
      response: { success: true, data: { recorded: true, confirmed: false } },
    });
    expect(line).toContain("POST /control/session-open");
  });

  /**
   * The fourth state, and the one the first cut could not express: the write
   * did NOT land. `record_open` returns early without writing when the map
   * lock is poisoned, and the backend's read-back sees the same poison — so
   * `recorded` is a measurement, not the constant `true` it used to be.
   *
   * It must not read as PROVISIONAL. Provisional says "the row is there,
   * waiting for a door"; this says "there is no row", and pointing a reader at
   * `POST /control/session-open` for it is advice that cannot work.
   */
  it("says NOT recorded — not provisional — when the write did not land", () => {
    const line = describeRecordOpenOutcome({
      claudeSessionId: "sess-5",
      terminalId: "t-5",
      response: {
        success: true,
        data: { recorded: false, confirmed: false, confirmBy: "POST /control/session-open" },
      },
    });
    expect(line).toContain("NOT recorded");
    expect(line).toContain("sess-5");
    expect(line).toContain("t-5");
    expect(line).not.toContain("PROVISIONAL");
    expect(line).not.toContain("BOUND");
    expect(line).not.toContain("UNKNOWN");
  });

  /**
   * `recorded: false` is a REPORT, not a parse failure — the three-way
   * UNKNOWN / not-recorded / recorded split only works if the reader keeps
   * them apart.
   */
  it("parses a not-recorded report rather than discarding it as unreadable", () => {
    expect(
      readRecordOpenReport({ success: true, data: { recorded: false, confirmed: false } }),
    ).toEqual({ recorded: false, confirmed: false, confirmBy: "" });
  });

  /**
   * A build predating the report resolves with `data: null`. That is UNKNOWN,
   * never "not confirmed" — collapsing it into PROVISIONAL would print a
   * confident claim about a runner that said nothing.
   */
  it("reports UNKNOWN — not provisional — when the build returns no report", () => {
    for (const response of [
      { success: true, data: null },
      { success: true },
      null,
      "not an object",
      { success: true, data: { recorded: "yes", confirmed: 1 } },
    ]) {
      expect(readRecordOpenReport(response)).toBeNull();
      const line = describeRecordOpenOutcome({
        claudeSessionId: "sess-4",
        terminalId: "t-4",
        response,
      });
      expect(line).toContain("UNKNOWN");
      expect(line).not.toContain("PROVISIONAL");
      expect(line).not.toContain("BOUND");
    }
  });
});
