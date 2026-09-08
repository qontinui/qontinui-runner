import { describe, it, expect } from "vitest";
import {
  applyRemoteMark,
  attachButtonState,
  attachErrorMessage,
  detachedRemoteTabs,
  fleetSessionAttachId,
  remoteBadgeLabel,
  remoteSessionLabel,
  remoteTabTitle,
  sameRemote,
  decodeHistoryBase64,
  savedRemoteSessionsToRestore,
  sessionLabelFromTitle,
  type RemoteTabIdentity,
} from "./remoteTabs";
import { findTerminalsOutsideProject } from "./useProjectTerminalReconcile";
import { isMarkableSession } from "./sessionDurability";
import type { FleetSession } from "./useFleetSessions";

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
    state: "working",
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

const remote: RemoteTabIdentity = {
  deviceId: "22222222-2222-2222-2222-222222222222",
  deviceLabel: "spaceship",
  sessionId: "11111111-1111-1111-1111-111111111111",
  remoteTerminalId: "t-remote",
  historyAvailable: true,
};

describe("attachButtonState (picker Attach button)", () => {
  it("is enabled for a live session on another device", () => {
    expect(attachButtonState(session(), true)).toEqual({ disabled: false, reason: null });
  });

  it("is disabled with a reason on the caller's own device", () => {
    const st = attachButtonState(session({ isCallerDevice: true }), true);
    expect(st.disabled).toBe(true);
    expect(st.reason).toMatch(/this machine/);
  });

  it("is disabled with a reason for a closed session", () => {
    expect(attachButtonState(session({ closedAt: "2026-09-07T00:00:00Z" }), true).disabled).toBe(
      true,
    );
    expect(attachButtonState(session({ state: "closed" }), true).reason).toMatch(/closed/);
  });

  it("is disabled when the device id is missing or the identity columns were degraded", () => {
    expect(attachButtonState(session({ deviceId: " " }), true).reason).toMatch(/which device/);
    expect(attachButtonState(session(), false).reason).toMatch(/could not read device identity/);
    // Unknown (null) presence is not treated as degraded — the id is on the row.
    expect(attachButtonState(session(), null).disabled).toBe(false);
  });

  it("names the ui-bridge id per session", () => {
    expect(fleetSessionAttachId("abc")).toBe("terminal.fleet-session-attach.abc");
  });
});

describe("attachErrorMessage (typed runner errors, shown inline)", () => {
  it("lifts the code out of the runner's remote_attach:<code>[:<reason>]: detail shape", () => {
    expect(attachErrorMessage("remote_attach:forbidden:preference_off: coord answered 403")).toBe(
      "forbidden (preference_off) — coord answered 403",
    );
    expect(attachErrorMessage(new Error("remote_attach:timeout: no reply within 20s"))).toBe(
      "timeout — no reply within 20s",
    );
  });

  it("passes an untyped message through and never returns an empty string", () => {
    expect(attachErrorMessage("relay closed")).toBe("relay closed");
    expect(attachErrorMessage("")).toMatch(/no reason/);
  });
});

describe("tab title and label", () => {
  it("prefers the work-unit slug, then intent, then repo@branch, then the id stem", () => {
    expect(remoteSessionLabel(session({ workUnitSlug: "plan-x", intent: "y" }))).toBe("plan-x");
    expect(remoteSessionLabel(session({ intent: "fix the thing" }))).toBe("fix the thing");
    expect(remoteSessionLabel(session({ repo: "r", branch: "b" }))).toBe("r @ b");
    expect(remoteSessionLabel(session())).toBe("11111111");
  });

  it("titles a remote tab '<device>: <session>' and can split it back", () => {
    const t = remoteTabTitle("spaceship", "plan-x");
    expect(t).toBe("spaceship: plan-x");
    expect(sessionLabelFromTitle(t, "spaceship")).toBe("plan-x");
    expect(sessionLabelFromTitle("renamed", "spaceship")).toBe("renamed");
  });
});

describe("remoteBadgeLabel (tab device badge)", () => {
  it("renders nothing for a local tab", () => {
    expect(remoteBadgeLabel(undefined)).toBeNull();
    expect(remoteBadgeLabel(null)).toBeNull();
  });

  it("shows the device label with both ids in the title", () => {
    const b = remoteBadgeLabel(remote);
    expect(b?.text).toBe("spaceship");
    expect(b?.title).toContain(remote.deviceId);
    expect(b?.title).toContain(remote.sessionId);
  });

  it("falls back to the device id stem when the label is blank", () => {
    expect(remoteBadgeLabel({ ...remote, deviceLabel: "  " })?.text).toBe("22222222");
  });
});

describe("applyRemoteMark / identity bookkeeping", () => {
  const tabs = [
    { id: "a", remote: undefined as RemoteTabIdentity | undefined },
    { id: "b", remote: undefined as RemoteTabIdentity | undefined },
  ];

  it("buffers a mark whose tab has not landed yet", () => {
    const r = applyRemoteMark(tabs, "zzz", remote);
    expect(r.buffered).toBe(true);
    expect(r.tabs).toBe(tabs);
  });

  it("applies the mark without touching other tabs, and is idempotent", () => {
    const r = applyRemoteMark(tabs, "b", remote);
    expect(r.buffered).toBe(false);
    expect(r.tabs[1].remote).toEqual(remote);
    expect(r.tabs[0]).toBe(tabs[0]);
    const again = applyRemoteMark(r.tabs, "b", remote);
    expect(again.tabs).toBe(r.tabs);
  });

  it("keys identity on (deviceId, sessionId) — a fresh remoteTerminalId is the same tab", () => {
    expect(sameRemote(remote, { ...remote, remoteTerminalId: "other" })).toBe(true);
    expect(sameRemote(remote, { ...remote, sessionId: "x" })).toBe(false);
    expect(sameRemote(remote, null)).toBe(false);
  });

  it("offers Reattach only for a remote tab whose pane ended", () => {
    const dead = { id: "d", isAlive: false, remote };
    const live = { id: "l", isAlive: true, remote };
    const localDead = { id: "x", isAlive: false, remote: undefined };
    expect(detachedRemoteTabs([dead, live, localDead])).toEqual([dead]);
  });
});

describe("restart: saved remote tabs become placeholders, never PTYs", () => {
  it("lists saved remote entries that are not live, once each", () => {
    const saved = [
      { title: "spaceship: plan-x", remote },
      { title: "dup", remote: { ...remote, remoteTerminalId: "t2" } },
      { title: "local shell" },
      { title: "other", remote: { ...remote, sessionId: "33333333" } },
    ];
    const out = savedRemoteSessionsToRestore(saved, [{ remote: undefined }]);
    expect(out.map((o) => o.remote.sessionId)).toEqual([remote.sessionId, "33333333"]);
    expect(out[0].title).toBe("spaceship: plan-x");
  });

  it("drops an entry once a live tab carries the same identity", () => {
    const out = savedRemoteSessionsToRestore([{ title: "t", remote }], [{ remote }]);
    expect(out).toEqual([]);
  });
});

describe("remote tabs are exempt from local-machine reconciles", () => {
  it("is never 'outside the project' — its cwd is another machine's", () => {
    const tabs = [
      { id: "r", title: "spaceship: x", workingDir: "/remote/cwd", isAlive: true, remote },
      { id: "l", title: "local", workingDir: "/elsewhere", isAlive: true },
    ];
    expect(findTerminalsOutsideProject(tabs, "/proj").map((t) => t.id)).toEqual(["l"]);
  });

  it("carries no local durability marker", () => {
    expect(isMarkableSession({ type: "terminal", remote })).toBe(false);
    expect(isMarkableSession({ type: "terminal" })).toBe(true);
  });
});

describe("sameRemote (tab identity is (deviceId, sessionId))", () => {
  const a: RemoteTabIdentity = {
    deviceId: "dev-a",
    sessionId: "sess-1",
    remoteTerminalId: "t-1",
    grantJti: "g-1",
  };

  it("is true for the same device+session even across a fresh attach", () => {
    // A reattach mints a new grant and a new remote terminal id; neither is
    // part of identity, or a reconnect would orphan the tab.
    expect(sameRemote(a, { ...a, remoteTerminalId: "t-2", grantJti: "g-2" })).toBe(true);
  });

  it("is false when either half differs", () => {
    expect(sameRemote(a, { ...a, deviceId: "dev-b" })).toBe(false);
    expect(sameRemote(a, { ...a, sessionId: "sess-2" })).toBe(false);
  });

  it("is false — never true — when either side is absent", () => {
    // A local tab has no remote identity; two local tabs must not compare
    // equal, or every local tab would alias every other one.
    expect(sameRemote(a, undefined)).toBe(false);
    expect(sameRemote(undefined, a)).toBe(false);
    expect(sameRemote(null, null)).toBe(false);
    expect(sameRemote(undefined, undefined)).toBe(false);
  });
});

describe("decodeHistoryBase64 (Phase 5 lazy scrollback)", () => {
  it("round-trips arbitrary bytes, not just text", () => {
    const bytes = new Uint8Array([0x00, 0x1b, 0x5b, 0x41, 0xff, 0x7f, 0x0a]);
    const b64 = btoa(String.fromCharCode(...bytes));
    expect(Array.from(decodeHistoryBase64(b64))).toEqual(Array.from(bytes));
  });

  it("decodes an empty body to an empty array, not a throw", () => {
    expect(decodeHistoryBase64("").length).toBe(0);
  });

  it("preserves length for bytes above the ASCII range", () => {
    // `charCodeAt` on a binary string yields 0..255; a naive TextDecoder path
    // would mangle these into multi-byte code points and change the length.
    const bytes = new Uint8Array([0xc3, 0xa9, 0xe2, 0x80, 0x94]);
    const out = decodeHistoryBase64(btoa(String.fromCharCode(...bytes)));
    expect(out.length).toBe(bytes.length);
    expect(Array.from(out)).toEqual(Array.from(bytes));
  });
});
