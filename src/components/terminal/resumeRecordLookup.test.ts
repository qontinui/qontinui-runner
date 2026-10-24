/**
 * Plan 2026-10-07-a-resumed-gate-continuation-loses-its-reporter-… Phase 3:
 * the two non-drain resume doors ("Retry resume", the zone-profile resume)
 * carry a gate continuation's identity from its durable record.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";

const mockInvoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
}));

import {
  buildProfileResumeCmd,
  buildRetryResumeArgs,
  fetchRecordsByClaudeSessionId,
  gateFromRecord,
} from "./resumeRecordLookup";
import type { TerminalSessionRecord } from "./types";
import type { SessionOpenArgs } from "./sessionRecordArgs";

const GATE = "7252781b-c49c-4a83-bdc2-4fec9c3dcd91";
const DEVICE = "0d4e6a2c-5b8f-4c1d-9e3a-7f6b2c8d1e40";

const rec = (overrides: Partial<TerminalSessionRecord>): TerminalSessionRecord => ({
  claudeSessionId: "sess-1",
  pageId: "default",
  zoneIndex: 0,
  terminalId: "tab-1",
  openedAt: 0,
  lastSeenAt: 0,
  state: "open",
  ...overrides,
});

const recordOpen: SessionOpenArgs = {
  claudeSessionId: "sess-1",
  configDir: undefined,
  workingDir: "/work",
  pageId: "default",
  zoneIndex: 0,
  title: "t",
  terminalId: "tab-1",
};

describe("buildRetryResumeArgs (test i — the hand retry carries the record's gate)", () => {
  it("passes the gate identity (and provider) when the record has one", () => {
    const args = buildRetryResumeArgs({
      tabId: "tab-1",
      claudeSessionId: "sess-1",
      configDir: "/cfg",
      record: rec({ gateId: GATE, gateConsumingDeviceId: DEVICE, provider: "claude" }),
      recordOpen,
    });
    expect(args.gate).toEqual({ gateId: GATE, gateConsumingDeviceId: DEVICE });
    expect(args.provider).toBe("claude");
    expect(args).toMatchObject({
      tabId: "tab-1",
      claudeSessionId: "sess-1",
      configDir: "/cfg",
      recordOpen,
    });
  });

  it("passes no gate for a record without one, or no record at all", () => {
    for (const record of [rec({}), undefined]) {
      const args = buildRetryResumeArgs({
        tabId: "tab-1",
        claudeSessionId: "sess-1",
        configDir: undefined,
        record,
        recordOpen,
      });
      expect("gate" in args).toBe(false);
      expect("provider" in args).toBe(false);
    }
  });

  it("gateFromRecord ignores a device id with no gate id", () => {
    expect(gateFromRecord(rec({ gateConsumingDeviceId: DEVICE }))).toBeUndefined();
  });
});

describe("buildProfileResumeCmd (test j — the profile resume uses the shared builder)", () => {
  beforeEach(() => mockInvoke.mockReset());

  it("asks the backend with the gate pair, CLAUDE_CONFIG_DIR and the summary policy", async () => {
    mockInvoke.mockResolvedValueOnce({ success: true, message: null, data: { command: "R" } });
    const cmd = await buildProfileResumeCmd(
      "sess-1",
      "/home/u/.claude-x",
      rec({ gateId: GATE, gateConsumingDeviceId: DEVICE }),
    );
    expect(cmd).toBe("R\r");
    expect(mockInvoke).toHaveBeenCalledWith("build_ai_resume_command", {
      sessionId: "sess-1",
      provider: "claude",
      configDir: "/home/u/.claude-x",
      isWindows: false,
      fullResume: false,
      gateId: GATE,
      gateDeviceId: DEVICE,
    });
  });

  it("a non-gate record sends no gate pair", async () => {
    mockInvoke.mockResolvedValueOnce({ success: true, message: null, data: { command: "R" } });
    await buildProfileResumeCmd("sess-1", undefined, rec({}));
    const args = mockInvoke.mock.calls[0][1] as Record<string, unknown>;
    expect(args).not.toHaveProperty("gateId");
    expect(args).not.toHaveProperty("gateDeviceId");
  });
});

describe("fetchRecordsByClaudeSessionId", () => {
  beforeEach(() => mockInvoke.mockReset());

  it("keys every listed record by claudeSessionId, across pages", async () => {
    mockInvoke.mockResolvedValueOnce({
      success: true,
      data: {
        sessions: [
          rec({ claudeSessionId: "a", pageId: "p1", gateId: GATE }),
          rec({ claudeSessionId: "b", pageId: "p2" }),
        ],
      },
    });
    const byId = await fetchRecordsByClaudeSessionId();
    expect(mockInvoke).toHaveBeenCalledWith("terminal_session_list_open");
    expect([...byId.keys()].sort()).toEqual(["a", "b"]);
    expect(byId.get("a")?.gateId).toBe(GATE);
  });

  it("is empty — never throws — on an IPC failure or a drain deferral", async () => {
    mockInvoke.mockRejectedValueOnce(new Error("ipc down"));
    expect((await fetchRecordsByClaudeSessionId()).size).toBe(0);
    mockInvoke.mockResolvedValueOnce({
      success: true,
      data: { sessions: [], deferredByDrain: { reason: "drained" } },
    });
    expect((await fetchRecordsByClaudeSessionId()).size).toBe(0);
  });
});
