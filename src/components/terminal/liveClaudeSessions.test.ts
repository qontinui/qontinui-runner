import { describe, it, expect } from "vitest";

import {
  extractLiveSessions,
  groupByAccount,
  sharedSessionIds,
  type LiveClaudeSession,
} from "./liveClaudeSessions";

function mk(over: Partial<LiveClaudeSession> = {}): LiveClaudeSession {
  return {
    sessionId: "b770ae37-1ffa-4888-a5d1-89d058307adf",
    name: "per-agent coord-mcp proxy",
    pid: 2804,
    account: { label: "paktis", wrapper: "clp" },
    workingDir: "D:/qontinui-root",
    status: "idle",
    kind: "interactive",
    startedAt: 1784712055852,
    updatedAt: 1784770342016,
    resumeCommand: "cd 'D:/qontinui-root' && clp --resume b770ae37-1ffa-4888-a5d1-89d058307adf",
    ...over,
  };
}

describe("extractLiveSessions", () => {
  it("unwraps the {sessions:[…]} envelope the command returns", () => {
    const got = extractLiveSessions({ sessions: [mk()] });
    expect(got).toHaveLength(1);
    expect(got[0].name).toBe("per-agent coord-mcp proxy");
  });

  it("accepts a bare array", () => {
    expect(extractLiveSessions([mk()])).toHaveLength(1);
  });

  it("degrades to [] rather than throwing on any other shape", () => {
    // Callers iterate the result during render; a throw here takes the panel
    // (or the whole command card) down with it.
    expect(extractLiveSessions(null)).toEqual([]);
    expect(extractLiveSessions(undefined)).toEqual([]);
    expect(extractLiveSessions("nope")).toEqual([]);
    expect(extractLiveSessions({})).toEqual([]);
    expect(extractLiveSessions({ sessions: "not-an-array" })).toEqual([]);
  });
});

describe("sharedSessionIds", () => {
  it("reports ids held by more than one live process", () => {
    // The restore-duplication symptom: one session id, several live processes,
    // each with its own auto-generated name.
    const sessions = [
      mk({ sessionId: "dup", pid: 10, name: "qontinui-web-0c" }),
      mk({ sessionId: "dup", pid: 11, name: "qontinui-web-07" }),
      mk({ sessionId: "solo", pid: 12, name: "qontinui-root-11" }),
    ];
    const shared = sharedSessionIds(sessions);
    expect([...shared.keys()]).toEqual(["dup"]);
    expect(shared.get("dup")!.map((s) => s.name)).toEqual(["qontinui-web-0c", "qontinui-web-07"]);
  });

  it("is empty when every id is unique", () => {
    expect(sharedSessionIds([mk({ sessionId: "a" }), mk({ sessionId: "b" })]).size).toBe(0);
  });

  it("is empty for no sessions", () => {
    expect(sharedSessionIds([]).size).toBe(0);
  });
});

describe("groupByAccount", () => {
  it("groups by label and preserves input order within a group", () => {
    const sessions = [
      mk({ account: { label: "gmail", wrapper: "clg" }, name: "a" }),
      mk({ account: { label: "paktis", wrapper: "clp" }, name: "b" }),
      mk({ account: { label: "gmail", wrapper: "clg" }, name: "c" }),
    ];
    const g = groupByAccount(sessions);
    expect([...g.keys()]).toEqual(["gmail", "paktis"]);
    expect(g.get("gmail")!.map((s) => s.name)).toEqual(["a", "c"]);
  });

  it("buckets a missing account under 'unknown' instead of throwing", () => {
    const s = mk();
    // Force the defensive path: the Rust side always sends `account`, but a
    // version skew must not crash the grouping.
    delete (s as { account?: unknown }).account;
    expect([...groupByAccount([s]).keys()]).toEqual(["unknown"]);
  });
});
