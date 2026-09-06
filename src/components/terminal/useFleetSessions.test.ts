import { describe, it, expect } from "vitest";
import {
  emptyReasonFor,
  isDegraded,
  groupByDevice,
  deviceLabel,
  type FleetSession,
  type FleetSessionsResponse,
} from "./useFleetSessions";
import {
  sessionStateLabel,
  sessionDescription,
  degradedNotice,
} from "./FleetSessionPicker";

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
  return {
    tenantId: "33333333-3333-3333-3333-333333333333",
    callerDeviceId: "22222222-2222-2222-2222-222222222222",
    sessions: [],
    count: 0,
    truncated: false,
    sessionBridgeColumnPresent: true,
    workAxisColumnsPresent: true,
    deviceIdentityColumnsPresent: true,
    ...over,
  };
}

describe("emptyReasonFor — an empty list must say WHY", () => {
  it("is null when there are rows", () => {
    expect(emptyReasonFor(true, null, [session()])).toBeNull();
  });

  it("reports not-loaded before any read completes", () => {
    expect(emptyReasonFor(false, null, [])).toBe("not-loaded");
  });

  it("reports error when the read failed — NOT observed-empty", () => {
    // The whole point: a failed read must never license the words
    // "no remote sessions".
    expect(emptyReasonFor(false, "boom", [])).toBe("error");
    expect(emptyReasonFor(true, "boom", [])).toBe("error");
  });

  it("reports observed-empty only for a completed, error-free read", () => {
    expect(emptyReasonFor(true, null, [])).toBe("observed-empty");
  });
});

describe("isDegraded — a degraded field is not an observation", () => {
  it("is false when coord read everything", () => {
    expect(isDegraded(response())).toBe(false);
  });

  it("is false for a null response (nothing read yet, not a degrade)", () => {
    expect(isDegraded(null)).toBe(false);
  });

  it.each([
    ["sessionBridgeColumnPresent"],
    ["workAxisColumnsPresent"],
    ["deviceIdentityColumnsPresent"],
  ] as const)("is true when %s is false", (flag) => {
    expect(isDegraded(response({ [flag]: false }))).toBe(true);
  });
});

describe("degradedNotice — the banner names what was unreadable", () => {
  it("is null when nothing is degraded", () => {
    expect(degradedNotice(response())).toBeNull();
    expect(degradedNotice(null)).toBeNull();
  });

  it("names each missing field and says unknown, not empty", () => {
    const n = degradedNotice(
      response({ workAxisColumnsPresent: false, deviceIdentityColumnsPresent: false }),
    );
    expect(n).toContain("work status");
    expect(n).toContain("device names");
    expect(n).not.toContain("harness session ids");
    expect(n).toContain("unknown, not empty");
  });
});

describe("groupByDevice", () => {
  it("puts the caller's own device first, then labels alphabetically", () => {
    const rows = [
      session({ sessionId: "s1", deviceId: "d-zeta", deviceHostname: "zeta" }),
      session({ sessionId: "s2", deviceId: "d-alpha", deviceHostname: "alpha" }),
      session({
        sessionId: "s3",
        deviceId: "d-local",
        deviceHostname: "local",
        isCallerDevice: true,
      }),
    ];
    const groups = groupByDevice(rows);
    expect(groups.map((g) => g.label)).toEqual(["local", "alpha", "zeta"]);
    expect(groups[0].isCallerDevice).toBe(true);
  });

  it("collects every session of a device into one group", () => {
    const rows = [
      session({ sessionId: "a", deviceId: "d1" }),
      session({ sessionId: "b", deviceId: "d1" }),
      session({ sessionId: "c", deviceId: "d2" }),
    ];
    const groups = groupByDevice(rows);
    expect(groups).toHaveLength(2);
    const d1 = groups.find((g) => g.deviceId === "d1")!;
    expect(d1.sessions.map((s) => s.sessionId)).toEqual(["a", "b"]);
  });

  it("is empty for no rows", () => {
    expect(groupByDevice([])).toEqual([]);
  });
});

describe("deviceLabel", () => {
  it("prefers display name, then hostname, then a shortened id", () => {
    expect(deviceLabel(session({ deviceDisplayName: "Big Box", deviceHostname: "bb01" }))).toBe(
      "Big Box",
    );
    expect(deviceLabel(session({ deviceHostname: "bb01" }))).toBe("bb01");
    expect(deviceLabel(session({ deviceId: "abcdef01-2222-3333-4444-555555555555" }))).toBe(
      "device abcdef01",
    );
  });

  it("treats a blank name as absent rather than rendering an empty label", () => {
    expect(deviceLabel(session({ deviceDisplayName: "   ", deviceHostname: "bb01" }))).toBe(
      "bb01",
    );
  });
});

describe("sessionStateLabel — the two axes stay distinct", () => {
  it("shows an em dash for an unknown liveness rather than inventing one", () => {
    expect(sessionStateLabel(session())).toBe("—");
  });

  it("shows liveness alone when the work axis is unknown", () => {
    expect(sessionStateLabel(session({ state: "working" }))).toBe("working");
  });

  it("shows both axes when both are known", () => {
    expect(sessionStateLabel(session({ state: "working", sessionStatus: "finished" }))).toBe(
      "working · finished",
    );
  });
});

describe("sessionDescription", () => {
  it("prefers the work-unit slug over free-text intent", () => {
    expect(
      sessionDescription(session({ workUnitSlug: "some-plan", intent: "poking about" })),
    ).toBe("some-plan");
  });

  it("falls back to intent, then repo@branch, then a stated absence", () => {
    expect(sessionDescription(session({ intent: "poking about" }))).toBe("poking about");
    expect(sessionDescription(session({ repo: "qontinui-coord", branch: "main" }))).toBe(
      "qontinui-coord @ main",
    );
    expect(sessionDescription(session({ repo: "qontinui-coord" }))).toBe("qontinui-coord");
    expect(sessionDescription(session())).toBe("(no declared work)");
  });
});
