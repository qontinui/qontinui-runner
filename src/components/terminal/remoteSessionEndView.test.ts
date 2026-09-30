/**
 * "End on remote" / "Close all finished" (plan
 * `2026-09-30-close-remote-sessions-from-the-local-runner`, Phase 5) — the
 * honesty rules, pinned on the pure module.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, it, expect } from "vitest";

import {
  CALLER_DEVICE_CHANGED_MESSAGE,
  CALLER_DEVICE_COLUMNS_UNAVAILABLE_MESSAGE,
  CALLER_DEVICE_UNKNOWN_MESSAGE,
  closeAllFinishedCandidates,
  closeAllFinishedLabel,
  describeEndResult,
  endButtonState,
  endInvokeFailure,
  endedHiddenMessage,
  isFinishedSessionStatus,
  runBounded,
  walkFinishedFleetSessions,
  type FinishedPageFetch,
} from "./remoteSessionEndView";
import { fleetErrorCode, fleetScopeKey } from "./fleetDiscovery";
import type { FleetSession, FleetSessionsResponse } from "./useFleetSessions";
import type { RemoteSessionEndResult } from "./remoteTabs";

const res = (patch: Partial<RemoteSessionEndResult>): RemoteSessionEndResult => ({
  outcome: "ended",
  deviceId: "dev-remote",
  sessionId: "sess-1",
  terminalId: "t-1",
  via: "graceful",
  reason: null,
  grantSource: "minted",
  ...patch,
});

const row = (patch: Partial<FleetSession>): FleetSession => ({
  sessionId: "s",
  deviceId: "dev-remote",
  isCallerDevice: false,
  deviceHostname: "box",
  deviceDisplayName: null,
  claudeCodeSessionId: null,
  sessionKind: null,
  intent: null,
  state: "active",
  sessionStatus: "finished",
  workUnitSlug: null,
  repo: null,
  branch: null,
  provider: null,
  correlationTopic: null,
  startedAt: null,
  lastHeartbeatAt: null,
  closedAt: null,
  ...patch,
});

const page = (
  sessions: FleetSession[],
  patch: Partial<FleetSessionsResponse> = {},
): FleetSessionsResponse => ({
  tenantId: "tenant",
  callerDeviceId: "dev-local",
  sessions,
  count: sessions.length,
  limit: 500,
  nextCursor: null,
  sessionBridgeColumnPresent: true,
  workAxisColumnsPresent: true,
  deviceIdentityColumnsPresent: true,
  ...patch,
});

describe("describeEndResult — an unknown is never rendered as ended", () => {
  it("renders `unknown` as unknown, not ended, and keeps the row", () => {
    const v = describeEndResult(res({ outcome: "unknown", via: null, reason: "timed out" }));
    expect(v.isEnded).toBe(false);
    expect(v.outcome).toBe("unknown");
    expect(v.label).toBe("unknown");
    expect(v.label).not.toMatch(/ended/);
    expect(v.headline).not.toMatch(/\bended\b/);
    expect(v.hidesRow).toBe(false);
    expect(v.offerForce).toBe(false);
    expect(v.detail).toBe("timed out");
  });

  it("treats an unrecognised wire outcome as unknown", () => {
    const v = describeEndResult(res({ outcome: "exited" as never }));
    expect(v.outcome).toBe("unknown");
    expect(v.isEnded).toBe(false);
  });

  it("an invoke that rejected is unknown carrying the error", () => {
    const r = endInvokeFailure(new Error("remote_session_end: bad id"), "d", "s");
    expect(r.outcome).toBe("unknown");
    const v = describeEndResult(r);
    expect(v.isEnded).toBe(false);
    expect(v.detail).toContain("bad id");
  });

  it("only a positive `ended` is ended, and says how", () => {
    expect(describeEndResult(res({ via: "graceful" })).isEnded).toBe(true);
    expect(describeEndResult(res({ via: "no_live_claude" })).headline).toMatch(/bare shell/);
    expect(describeEndResult(res({ via: "force" })).headline).toMatch(/force/);
    expect(describeEndResult(res({})).hidesRow).toBe(true);
  });

  it("`refused` shows the reason and offers Force end", () => {
    const v = describeEndResult(
      res({ outcome: "refused", via: null, reason: "Refused: unsent draft in prompt" }),
    );
    expect(v.isEnded).toBe(false);
    expect(v.offerForce).toBe(true);
    expect(v.detail).toBe("Refused: unsent draft in prompt");
    expect(v.hidesRow).toBe(false);
  });

  it("`refused` with no reason still says so rather than rendering nothing", () => {
    expect(describeEndResult(res({ outcome: "refused", reason: null })).detail).toMatch(
      /no reason/,
    );
  });

  it("`still_running` says wedged and offers Force end", () => {
    const v = describeEndResult(res({ outcome: "still_running", via: null }));
    expect(v.headline).toMatch(/still running/);
    expect(v.headline).toMatch(/wedged/);
    expect(v.offerForce).toBe(true);
  });

  it("a result that already WAS a force offers no further force", () => {
    expect(describeEndResult(res({ outcome: "refused" }), true).offerForce).toBe(false);
    expect(describeEndResult(res({ outcome: "still_running" }), true).offerForce).toBe(false);
  });

  it("an `end_already_running` refusal reads as already ending, offers no force", () => {
    const v = describeEndResult(
      res({
        outcome: "refused",
        reason: "end_already_running: an end for terminal t-1 is already running on this device",
      }),
    );
    expect(v.label).toBe("already ending");
    expect(v.isEnded).toBe(false);
    expect(v.offerForce).toBe(false);
    expect(v.headline).not.toMatch(/still running/);
  });

  it("`not_found` is 'already gone', hides the row, and is not ended", () => {
    const v = describeEndResult(res({ outcome: "not_found", via: null }));
    expect(v.label).toBe("already gone");
    expect(v.isEnded).toBe(false);
    expect(v.hidesRow).toBe(true);
  });
});

describe("isFinishedSessionStatus — the client-side guard", () => {
  it("accepts finished and the legacy done", () => {
    expect(isFinishedSessionStatus("finished")).toBe(true);
    expect(isFinishedSessionStatus(" Done ")).toBe(true);
  });
  it("rejects everything else, including null", () => {
    for (const v of ["working", "waiting_human", "", null, undefined]) {
      expect(isFinishedSessionStatus(v)).toBe(false);
    }
  });
});

describe("closeAllFinishedCandidates — N counts REMOTE finished sessions only", () => {
  const sessions = [
    row({ sessionId: "remote-finished" }),
    row({ sessionId: "remote-done", sessionStatus: "done" }),
    row({ sessionId: "local-finished", isCallerDevice: true, deviceId: "dev-local" }),
    row({ sessionId: "remote-working", sessionStatus: "working" }),
    row({ sessionId: "remote-null", sessionStatus: null }),
    row({ sessionId: "remote-closed", closedAt: "2026-09-30T00:00:00Z" }),
    row({ sessionId: "remote-no-device", deviceId: "" }),
    row({ sessionId: "remote-ended-here" }),
  ];

  it("excludes this device, non-finished, closed, unaddressable and already-ended rows", () => {
    const ids = closeAllFinishedCandidates(
      sessions,
      new Set(["remote-ended-here"]),
      "dev-local",
    ).map((s) => s.sessionId);
    expect(ids).toEqual(["remote-finished", "remote-done"]);
  });

  it("excludes a row on the caller's device even when coord says isCallerDevice: false", () => {
    const rows = [
      row({ sessionId: "remote-finished" }),
      row({ sessionId: "mislabelled-local", deviceId: "dev-local", isCallerDevice: false }),
      row({ sessionId: "padded-local", deviceId: " dev-local ", isCallerDevice: false }),
    ];
    const ids = closeAllFinishedCandidates(rows, new Set(), "dev-local").map((s) => s.sessionId);
    expect(ids).toEqual(["remote-finished"]);
  });

  it("offers nothing when the caller device is unknown", () => {
    for (const caller of [null, undefined, "", "  "]) {
      expect(closeAllFinishedCandidates(sessions, new Set(), caller)).toEqual([]);
    }
  });
});

describe("walkFinishedFleetSessions", () => {
  it("asks coord for session_status=finished and pages through cursors", async () => {
    const seen: Array<{ sessionStatus: string; cursor: string | null }> = [];
    const fetch: FinishedPageFetch = async ({ sessionStatus, cursor }) => {
      seen.push({ sessionStatus, cursor });
      return cursor === null
        ? page([row({ sessionId: "a" })], { nextCursor: "c1" })
        : page([row({ sessionId: "b" })]);
    };
    const read = await walkFinishedFleetSessions(fetch);
    expect(seen).toEqual([
      { sessionStatus: "finished", cursor: null },
      { sessionStatus: "finished", cursor: "c1" },
    ]);
    expect(read.kind).toBe("ok");
    if (read.kind !== "ok") return;
    expect(read.sessions.map((s) => s.sessionId)).toEqual(["a", "b"]);
    expect(read.capped).toBe(false);
  });

  it("filters client-side: a stale coord that ignores the param cannot offer non-finished rows", async () => {
    const fetch: FinishedPageFetch = async () =>
      page([
        row({ sessionId: "fin" }),
        row({ sessionId: "busy", sessionStatus: "working" }),
        row({ sessionId: "unset", sessionStatus: null }),
      ]);
    const read = await walkFinishedFleetSessions(fetch);
    expect(read.kind === "ok" && read.sessions.map((s) => s.sessionId)).toEqual(["fin"]);
  });

  it("a 503 work_axis_columns_absent renders 'unavailable', never zero", async () => {
    const fetch: FinishedPageFetch = async () => {
      throw 'GET /coord/sessions/fleet returned 503 — body: {"error":"work_axis_columns_absent","detail":"x"}';
    };
    const read = await walkFinishedFleetSessions(fetch);
    expect(read.kind).toBe("unavailable");
    const label = closeAllFinishedLabel(read, 0);
    expect(label).toBe("Close all finished (unavailable)");
    expect(label).not.toMatch(/\(0\)/);
    if (read.kind === "unavailable") expect(read.message).toMatch(/unavailable/);
  });

  it("a 503 work_axis_columns_unknown also renders 'unavailable', never zero", async () => {
    const read = await walkFinishedFleetSessions(async () => {
      throw 'GET /coord/sessions/fleet returned 503 — body: {"error":"work_axis_columns_unknown"}';
    });
    expect(read.kind).toBe("unavailable");
    expect(closeAllFinishedLabel(read, 0)).toBe("Close all finished (unavailable)");
  });

  it("a page served with degraded work-axis columns is unavailable, not an empty set", async () => {
    const fetch: FinishedPageFetch = async () =>
      page([row({ sessionId: "x", sessionStatus: null })], { workAxisColumnsPresent: false });
    const read = await walkFinishedFleetSessions(fetch);
    expect(read.kind).toBe("unavailable");
  });

  it("an ok read carries the caller device it was read as", async () => {
    const read = await walkFinishedFleetSessions(async () => page([row({ sessionId: "a" })]));
    expect(read.kind === "ok" && read.callerDeviceId).toBe("dev-local");
  });

  it("an unknown caller device is unavailable, never candidates", async () => {
    for (const callerDeviceId of [null, "", "  "]) {
      const read = await walkFinishedFleetSessions(async () =>
        page([row({ sessionId: "a" })], { callerDeviceId }),
      );
      expect(read.kind).toBe("unavailable");
      expect(closeAllFinishedLabel(read, 0)).toBe("Close all finished (unavailable)");
      if (read.kind === "unavailable") expect(read.message).toBe(CALLER_DEVICE_UNKNOWN_MESSAGE);
    }
  });

  it("degraded device identity columns are unavailable, never candidates", async () => {
    const read = await walkFinishedFleetSessions(async () =>
      page([row({ sessionId: "a" })], { deviceIdentityColumnsPresent: false }),
    );
    expect(read.kind).toBe("unavailable");
    expect(closeAllFinishedLabel(read, 0)).toBe("Close all finished (unavailable)");
    if (read.kind === "unavailable") {
      expect(read.message).toBe(CALLER_DEVICE_COLUMNS_UNAVAILABLE_MESSAGE);
    }
  });

  it("a later page that names a different caller device is unavailable", async () => {
    const read = await walkFinishedFleetSessions(async ({ cursor }) =>
      cursor === null
        ? page([row({ sessionId: "a" })], { nextCursor: "c1" })
        : page([row({ sessionId: "b" })], { callerDeviceId: "dev-other" }),
    );
    expect(read.kind).toBe("unavailable");
    if (read.kind === "unavailable") expect(read.message).toBe(CALLER_DEVICE_CHANGED_MESSAGE);
  });

  it("a transport failure is an error with no number", async () => {
    const read = await walkFinishedFleetSessions(async () => {
      throw "GET https://coord/coord/sessions/fleet: connection refused";
    });
    expect(read.kind).toBe("error");
    expect(closeAllFinishedLabel(read, 0)).toBe("Close all finished (?)");
  });

  it("hitting the page bound is truncated and the count is a floor (N+)", async () => {
    let n = 0;
    const read = await walkFinishedFleetSessions(
      async () => page([row({ sessionId: `s${n++}` })], { nextCursor: `c${n}` }),
      { maxPages: 2 },
    );
    expect(read.kind === "ok" && read.capped).toBe(true);
    expect(closeAllFinishedLabel(read, 2)).toBe("Close all finished (2+)");
  });

  it("a cursor handed back unchanged stops the walk as truncated", async () => {
    const read = await walkFinishedFleetSessions(async ({ cursor }) =>
      page([row({ sessionId: "a" })], { nextCursor: cursor ?? "same" }),
    );
    expect(read.kind === "ok" && read.capped).toBe(true);
  });
});

describe("fleet discovery learns the Phase 4 codes and scope", () => {
  it("recognises both new error codes", () => {
    expect(fleetErrorCode('returned 400 — body: {"error":"unknown_session_status"}')).toBe(
      "unknown_session_status",
    );
    expect(fleetErrorCode('returned 503 — body: {"error":"work_axis_columns_absent"}')).toBe(
      "work_axis_columns_absent",
    );
  });

  it("puts sessionStatus in the cursor scope", () => {
    const base = { deviceId: null, state: null, includeClosed: false };
    expect(fleetScopeKey({ ...base, sessionStatus: "finished" })).not.toBe(fleetScopeKey(base));
    expect(fleetScopeKey({ ...base, sessionStatus: null })).toBe(fleetScopeKey(base));
  });
});

describe("endButtonState", () => {
  it("is disabled for this device's own sessions", () => {
    expect(endButtonState(row({ isCallerDevice: true }), true).disabled).toBe(true);
  });
  it("is enabled for an addressable remote session", () => {
    expect(endButtonState(row({}), true)).toEqual({ disabled: false, reason: null });
  });
  it("is disabled when coord could not vouch for the device id", () => {
    expect(endButtonState(row({}), false).disabled).toBe(true);
  });
});

describe("runBounded", () => {
  it("never has more than `limit` in flight and settles every item", async () => {
    let inFlight = 0;
    let peak = 0;
    const done: number[] = [];
    await runBounded(
      [1, 2, 3, 4, 5, 6, 7],
      3,
      async (x) => {
        inFlight += 1;
        peak = Math.max(peak, inFlight);
        await new Promise((r) => setTimeout(r, 5));
        inFlight -= 1;
        return x * 2;
      },
      (_x, r) => done.push(r),
    );
    expect(peak).toBe(3);
    expect(done.sort((a, b) => a - b)).toEqual([2, 4, 6, 8, 10, 12, 14]);
  });
});

describe("endedHiddenMessage", () => {
  it("says the fleet list may still show them", () => {
    expect(endedHiddenMessage(0)).toBeNull();
    expect(endedHiddenMessage(2)).toMatch(/may still show them/);
  });
});

describe("wiring", () => {
  const read = (f: string) => readFileSync(fileURLToPath(new URL(f, import.meta.url)), "utf8");

  it("Close on a remote tab still means detach — the close handler is unchanged", () => {
    const src = read("./ZoneHoverActions.tsx");
    expect(src).toContain("closeTerminal(tabId);");
    expect(src).toContain("<RemoteTabEndAction tab={remoteTab} />");
  });

  it("the bulk confirm re-reads first and runs over exactly the list it showed", () => {
    const picker = read("./FleetSessionPicker.tsx");
    expect(picker).toContain("const read = await refreshFinished();");
    expect(picker).toMatch(
      /closeAllFinishedCandidates\(\s*read\.sessions,\s*endedHere,\s*read\.callerDeviceId,?\s*\)/,
    );
    const dialog = read("./CloseAllFinishedDialog.tsx");
    // The run iterates the `items` prop — no fetch inside the dialog.
    expect(dialog).toContain("await runBounded(\n      items,");
    expect(dialog).not.toContain("fleet_sessions_list");
  });

  it("the bulk run is graceful only", () => {
    const src = read("./CloseAllFinishedDialog.tsx");
    expect(src).toContain("remoteSessionEnd(it.deviceId, it.sessionId, false)");
    expect(src).not.toMatch(/remoteSessionEnd\([^)]*true\)/);
    expect(src).toContain("CLOSE_ALL_FINISHED_CONCURRENCY");
  });

  it("the picker offers the per-row end only for remote rows", () => {
    const src = read("./FleetSessionPicker.tsx");
    expect(src).toMatch(/\{!s\.isCallerDevice && \(\s*<button[\s\S]{0,200}fleetSessionEndId/);
  });
});
