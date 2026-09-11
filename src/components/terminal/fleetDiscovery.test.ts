/**
 * Unit tests for the Fleet picker's discovery logic (plan
 * `2026-09-11-headless-runner-parity-from-a-headed-runner`, Phase 2).
 *
 * The properties under test are the ones the defect turned on: a truncated read
 * must be reachable, a filtered list must never read as a total, and the
 * client-side text filter must never be mistaken for one that reaches coord.
 */

import { describe, it, expect } from "vitest";

import {
  DEFAULT_FLEET_SERVER_FILTER,
  FLEET_DEFAULT_LIMIT,
  FLEET_LIMIT_LADDER,
  FLEET_MAX_LIMIT,
  FLEET_STATE_VOCABULARY,
  fleetCountSummary,
  fleetFilterConflict,
  fleetFilteredOutMessage,
  fleetSearchTerms,
  fleetSessionHaystack,
  fleetSessionMatchesTerms,
  fleetStateOptions,
  fleetTruncation,
  filterFleetSessions,
  hasActiveFleetFilter,
  hasNarrowingFleetFilter,
  mergeDeviceCatalog,
  nextFleetLimit,
  type FleetDeviceOption,
} from "./fleetDiscovery";
import { devicesSeenIn, type FleetSession, type FleetSessionsResponse } from "./useFleetSessions";

function session(over: Partial<FleetSession> = {}): FleetSession {
  return {
    sessionId: "11111111-1111-1111-1111-111111111111",
    deviceId: "22222222-2222-2222-2222-222222222222",
    isCallerDevice: false,
    deviceHostname: null,
    deviceDisplayName: null,
    claudeCodeSessionId: null,
    sessionKind: null,
    intent: null,
    state: null,
    sessionStatus: null,
    workUnitSlug: null,
    repo: null,
    branch: null,
    provider: null,
    correlationTopic: null,
    startedAt: null,
    lastHeartbeatAt: null,
    closedAt: null,
    ...over,
  };
}

function response(over: Partial<FleetSessionsResponse> = {}): FleetSessionsResponse {
  const sessions = over.sessions ?? [];
  return {
    tenantId: "33333333-3333-3333-3333-333333333333",
    callerDeviceId: "22222222-2222-2222-2222-222222222222",
    count: sessions.length,
    truncated: false,
    sessionBridgeColumnPresent: true,
    workAxisColumnsPresent: true,
    deviceIdentityColumnsPresent: true,
    ...over,
    sessions,
  };
}

describe("nextFleetLimit — the page ladder ends at coord's ceiling", () => {
  it("walks the ladder upward from coord's default page", () => {
    expect(nextFleetLimit(FLEET_DEFAULT_LIMIT)).toBe(250);
    expect(nextFleetLimit(250)).toBe(FLEET_MAX_LIMIT);
  });

  it("returns null at the ceiling — no larger read exists, so none is offered", () => {
    expect(nextFleetLimit(FLEET_MAX_LIMIT)).toBeNull();
  });

  it("returns null above the ceiling, which coord would clamp anyway", () => {
    expect(nextFleetLimit(FLEET_MAX_LIMIT + 1)).toBeNull();
    expect(nextFleetLimit(10_000)).toBeNull();
  });

  it("lifts an off-ladder limit onto the next rung rather than stalling", () => {
    expect(nextFleetLimit(1)).toBe(FLEET_DEFAULT_LIMIT);
    expect(nextFleetLimit(101)).toBe(250);
    expect(nextFleetLimit(499)).toBe(FLEET_MAX_LIMIT);
  });

  it("never proposes a limit coord would clamp", () => {
    for (const rung of FLEET_LIMIT_LADDER) {
      expect(rung).toBeLessThanOrEqual(FLEET_MAX_LIMIT);
    }
    let at = 1;
    for (let i = 0; i < 20; i += 1) {
      const next = nextFleetLimit(at);
      if (next === null) break;
      expect(next).toBeGreaterThan(at);
      expect(next).toBeLessThanOrEqual(FLEET_MAX_LIMIT);
      at = next;
    }
    expect(at).toBe(FLEET_MAX_LIMIT);
  });
});

describe("fleetTruncation — completeness is classified, never assumed", () => {
  it("is UNKNOWN before any read completes — not 'none'", () => {
    // The distinction this phase exists for: no answer is not a complete answer.
    expect(fleetTruncation(null, FLEET_DEFAULT_LIMIT)).toEqual({ kind: "unknown" });
  });

  it("is 'none' when coord served every matching row", () => {
    const r = response({ sessions: [session()], truncated: false });
    expect(fleetTruncation(r, FLEET_DEFAULT_LIMIT)).toEqual({ kind: "none" });
  });

  it("offers the next larger read while one exists", () => {
    const rows = Array.from({ length: 100 }, (_, i) =>
      session({ sessionId: `s-${i}`, workUnitSlug: `plan-${i}` }),
    );
    const t = fleetTruncation(response({ sessions: rows, truncated: true }), FLEET_DEFAULT_LIMIT);

    expect(t.kind).toBe("more-available");
    if (t.kind !== "more-available") throw new Error("unreachable");
    expect(t.shown).toBe(100);
    expect(t.nextLimit).toBe(250);
    expect(t.message).toContain("Load 250");
  });

  it("at the ceiling says no larger read exists and names what does work", () => {
    const rows = Array.from({ length: FLEET_MAX_LIMIT }, (_, i) =>
      session({ sessionId: `s-${i}` }),
    );
    const t = fleetTruncation(response({ sessions: rows, truncated: true }), FLEET_MAX_LIMIT);

    expect(t.kind).toBe("at-ceiling");
    if (t.kind !== "at-ceiling") throw new Error("unreachable");
    expect(t.message).toContain(String(FLEET_MAX_LIMIT));
    expect(t.message).toMatch(/narrow by device or state/i);
    // And it must say the text box does NOT reach past the page, since at this
    // point it is the only other control on screen.
    expect(t.message).toMatch(/already loaded/i);
  });

  it("a full page that is NOT truncated is complete — count alone never decides", () => {
    const rows = Array.from({ length: FLEET_DEFAULT_LIMIT }, (_, i) =>
      session({ sessionId: `s-${i}` }),
    );
    expect(
      fleetTruncation(response({ sessions: rows, truncated: false }), FLEET_DEFAULT_LIMIT),
    ).toEqual({
      kind: "none",
    });
  });
});

describe("text filtering — AND over every searchable field", () => {
  const rows = [
    session({
      sessionId: "aaaa1111-0000-0000-0000-000000000001",
      workUnitSlug: "coord-merge-train-fix",
      repo: "qontinui-coord",
      branch: "main",
      state: "active",
    }),
    session({
      sessionId: "bbbb2222-0000-0000-0000-000000000002",
      intent: "rewrite the fleet picker",
      repo: "qontinui-web",
      branch: "feat/picker",
      state: "stale",
      provider: "claude",
    }),
    session({
      sessionId: "cccc3333-0000-0000-0000-000000000003",
      repo: "qontinui-web",
      branch: "main",
      deviceHostname: "merytshost",
      state: "active",
    }),
  ];

  it("an empty query keeps every row (and the same array)", () => {
    expect(filterFleetSessions(rows, "")).toBe(rows);
    expect(filterFleetSessions(rows, "   ")).toBe(rows);
  });

  it("matches case-insensitively across fields", () => {
    expect(filterFleetSessions(rows, "MERGE-TRAIN")).toHaveLength(1);
    expect(filterFleetSessions(rows, "merytshost")).toHaveLength(1);
  });

  it("ANDs terms, so every added word narrows", () => {
    expect(filterFleetSessions(rows, "qontinui-web")).toHaveLength(2);
    expect(filterFleetSessions(rows, "qontinui-web main")).toHaveLength(1);
    expect(filterFleetSessions(rows, "qontinui-web main nonsense")).toHaveLength(0);
  });

  it("finds a row by a pasted session id", () => {
    const hit = filterFleetSessions(rows, "bbbb2222-0000-0000-0000-000000000002");
    expect(hit).toHaveLength(1);
    expect(hit[0]?.repo).toBe("qontinui-web");
  });

  it("never matches the literal string 'null' for an absent field", () => {
    // coord's nulls are UNKNOWN; inventing the word "null" for them would let a
    // search match rows on a value they do not have.
    const bare = session();
    expect(fleetSessionHaystack(bare)).not.toContain("null");
    expect(fleetSessionMatchesTerms(bare, ["null"])).toBe(false);
  });

  it("splits a query on any run of whitespace", () => {
    expect(fleetSearchTerms("  a\t b  c ")).toEqual(["a", "b", "c"]);
    expect(fleetSearchTerms("")).toEqual([]);
  });
});

describe("mergeDeviceCatalog — the device list only ever grows", () => {
  const a: FleetDeviceOption = { deviceId: "a", label: "alpha", isCallerDevice: false };
  const b: FleetDeviceOption = { deviceId: "b", label: "bravo", isCallerDevice: true };

  it("keeps devices a narrowed read no longer returns", () => {
    // This is the trap the merge exists for: after selecting device `a`, coord's
    // next response holds only `a`, and recomputing would leave no way back.
    const merged = mergeDeviceCatalog([a, b], [a]);
    expect(merged.map((d) => d.deviceId).sort()).toEqual(["a", "b"]);
  });

  it("puts the caller's own device first, then sorts by label", () => {
    const merged = mergeDeviceCatalog(
      [],
      [{ deviceId: "z", label: "zulu", isCallerDevice: false }, a, b],
    );
    expect(merged.map((d) => d.deviceId)).toEqual(["b", "a", "z"]);
  });

  it("lets a real label replace the degraded-read id placeholder", () => {
    const placeholder: FleetDeviceOption = {
      deviceId: "a",
      label: "device abcd1234",
      isCallerDevice: false,
    };
    const merged = mergeDeviceCatalog([placeholder], [a]);
    expect(merged[0]?.label).toBe("alpha");
  });

  it("does not let a placeholder overwrite a real label", () => {
    const placeholder: FleetDeviceOption = {
      deviceId: "a",
      label: "device abcd1234",
      isCallerDevice: false,
    };
    const merged = mergeDeviceCatalog([a], [placeholder]);
    expect(merged[0]?.label).toBe("alpha");
  });

  it("is idempotent", () => {
    const once = mergeDeviceCatalog([], [a, b]);
    expect(mergeDeviceCatalog(once, [a, b])).toEqual(once);
  });
});

describe("devicesSeenIn — the filter options a page of rows supports", () => {
  it("returns one entry per device, labelled", () => {
    const seen = devicesSeenIn([
      session({ deviceId: "a", deviceDisplayName: "alpha" }),
      session({ deviceId: "a", deviceDisplayName: "alpha" }),
      session({ deviceId: "b", deviceHostname: "bravo-host", isCallerDevice: true }),
    ]);
    expect(seen).toEqual([
      { deviceId: "a", label: "alpha", isCallerDevice: false },
      { deviceId: "b", label: "bravo-host", isCallerDevice: true },
    ]);
  });

  it("is empty for an empty page — which the caller must not merge as fact", () => {
    expect(devicesSeenIn([])).toEqual([]);
  });

  it("feeds mergeDeviceCatalog without losing a previously seen device", () => {
    // The end-to-end shape of the trap: read everything, then narrow to one
    // device, and the dropdown must still offer the other.
    const all = devicesSeenIn([
      session({ deviceId: "a", deviceDisplayName: "alpha" }),
      session({ deviceId: "b", deviceDisplayName: "bravo" }),
    ]);
    const narrowed = devicesSeenIn([session({ deviceId: "a", deviceDisplayName: "alpha" })]);
    const catalog = mergeDeviceCatalog(mergeDeviceCatalog([], all), narrowed);
    expect(catalog.map((d) => d.deviceId)).toEqual(["a", "b"]);
  });
});

describe("fleetStateOptions — the known vocabulary, plus whatever coord served", () => {
  it("offers coord's whole vocabulary even when no row carries a value", () => {
    expect(fleetStateOptions([])).toEqual([...FLEET_STATE_VOCABULARY]);
  });

  it("appends a state coord emitted that this build does not know about", () => {
    // The vocabulary is enforced in Rust, not by a DB constraint, so it can
    // evolve without a migration — a pinned list would silently hide the new
    // value, which is the same quiet omission this phase is fixing.
    const opts = fleetStateOptions([session({ state: "quiescing" }), session({ state: "active" })]);
    expect(opts).toContain("quiescing");
    expect(opts.filter((v) => v === "active")).toHaveLength(1);
  });

  it("ignores blank states rather than offering an unselectable option", () => {
    expect(fleetStateOptions([session({ state: "  " })])).toEqual([...FLEET_STATE_VOCABULARY]);
  });
});

describe("hasActiveFleetFilter — drives whether a reset is offered at all", () => {
  it("is false for the picker's default state", () => {
    expect(hasActiveFleetFilter(DEFAULT_FLEET_SERVER_FILTER, "")).toBe(false);
  });

  it.each([
    ["device", { ...DEFAULT_FLEET_SERVER_FILTER, deviceId: "a" }, ""],
    ["state", { ...DEFAULT_FLEET_SERVER_FILTER, state: "active" }, ""],
    ["closed", { ...DEFAULT_FLEET_SERVER_FILTER, includeClosed: true }, ""],
    ["limit", { ...DEFAULT_FLEET_SERVER_FILTER, limit: 250 }, ""],
    ["text", DEFAULT_FLEET_SERVER_FILTER, "web"],
  ])("is true when %s is set", (_name, server, text) => {
    expect(hasActiveFleetFilter(server, text)).toBe(true);
  });

  it("treats a whitespace-only query as no filter", () => {
    expect(hasActiveFleetFilter(DEFAULT_FLEET_SERVER_FILTER, "   ")).toBe(false);
  });
});

describe("fleetCountSummary — a filtered subset is never shown as a total", () => {
  it("reports a plain count when nothing is filtered out", () => {
    expect(fleetCountSummary({ matched: 3, loaded: 3, devices: 2, remote: 1 })).toBe(
      "3 sessions on 2 devices · 1 remote",
    );
  });

  it("says 'of N loaded' the moment a filter hides anything", () => {
    expect(fleetCountSummary({ matched: 2, loaded: 47, devices: 1, remote: 0 })).toBe(
      "2 of 47 loaded on 1 device",
    );
  });

  it("gets the singular right", () => {
    expect(fleetCountSummary({ matched: 1, loaded: 1, devices: 1, remote: 0 })).toBe(
      "1 session on 1 device",
    );
  });

  it("omits the remote clause when every row is local", () => {
    expect(fleetCountSummary({ matched: 2, loaded: 2, devices: 1, remote: 0 })).not.toContain(
      "remote",
    );
  });
});

describe("fleetFilteredOutMessage — 'your filter matched nothing' ≠ 'the fleet is empty'", () => {
  it("explains a text filter that hid every loaded row", () => {
    const msg = fleetFilteredOutMessage(42, 0, "zzz");
    expect(msg).toContain("“zzz”");
    expect(msg).toContain("42 sessions are loaded");
    expect(msg).toMatch(/filters only those/i);
  });

  it("is silent when rows are visible", () => {
    expect(fleetFilteredOutMessage(42, 5, "zzz")).toBeNull();
  });

  it("is silent when the read itself was empty — that answer belongs elsewhere", () => {
    // `emptyReasonFor` owns that case: it is the only thing that can tell a
    // failed read from an observed-empty fleet.
    expect(fleetFilteredOutMessage(0, 0, "zzz")).toBeNull();
  });

  it("is silent when there is no text query to blame", () => {
    expect(fleetFilteredOutMessage(42, 0, "  ")).toBeNull();
  });
});

describe("fleetFilterConflict — a self-starving filter pair is named, not silently obeyed", () => {
  it("flags state=closed while closed sessions are excluded", () => {
    // coord appends `AND s.closed_at IS NULL` for include_closed=false, so the
    // pair very nearly excludes itself and the empty list reads as a fact about
    // the fleet rather than about the filters.
    const msg = fleetFilterConflict({
      ...DEFAULT_FLEET_SERVER_FILTER,
      state: "closed",
    });
    expect(msg).toMatch(/closed/i);
    expect(msg).toMatch(/turn on/i);
  });

  it("is silent once closed sessions are included", () => {
    expect(
      fleetFilterConflict({
        ...DEFAULT_FLEET_SERVER_FILTER,
        state: "closed",
        includeClosed: true,
      }),
    ).toBeNull();
  });

  it("is silent for every other state", () => {
    for (const state of ["active", "stale", "expected", "pending_resolution", null]) {
      expect(fleetFilterConflict({ ...DEFAULT_FLEET_SERVER_FILTER, state })).toBeNull();
    }
  });
});

describe("hasNarrowingFleetFilter — only a narrowing filter can explain an empty list", () => {
  it("is false for the default state", () => {
    expect(hasNarrowingFleetFilter(DEFAULT_FLEET_SERVER_FILTER, "")).toBe(false);
  });

  it.each([
    ["device", { ...DEFAULT_FLEET_SERVER_FILTER, deviceId: "a" }, ""],
    ["state", { ...DEFAULT_FLEET_SERVER_FILTER, state: "active" }, ""],
    ["text", DEFAULT_FLEET_SERVER_FILTER, "web"],
  ])("is true when %s narrows the read", (_name, server, text) => {
    expect(hasNarrowingFleetFilter(server, text)).toBe(true);
  });

  it.each([
    ["a raised limit", { ...DEFAULT_FLEET_SERVER_FILTER, limit: 250 }],
    ["including closed sessions", { ...DEFAULT_FLEET_SERVER_FILTER, includeClosed: true }],
  ])("is false for %s, which WIDENS the read", (_name, server) => {
    // Blaming an empty result on a filter that widened the query would be a
    // false explanation of an honest zero.
    expect(hasNarrowingFleetFilter(server, "")).toBe(false);
    expect(hasActiveFleetFilter(server, "")).toBe(true);
  });
});
