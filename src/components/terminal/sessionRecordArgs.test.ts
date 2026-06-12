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
import { buildSessionOpenArgs, resolveZoneIndex } from "./sessionRecordArgs";
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

  // Omitted bindOrigin → key ABSENT so the backend preserves any existing origin.
  it("includes bindOrigin only when the caller asserts one", () => {
    const base = {
      assignments: {},
      tabs: [],
      tabId: "t",
      claudeSessionId: "sid",
      configDir: undefined,
      pageId: "default",
    };
    expect("bindOrigin" in buildSessionOpenArgs(base)).toBe(false);
    expect(buildSessionOpenArgs({ ...base, bindOrigin: "pinned" }).bindOrigin).toBe("pinned");
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

describe("buildSessionCloseRecord — explicit-close recording", () => {
  it("returns explicit-close args for a tab carrying a claudeSessionId", () => {
    const tabs = [tab("shell"), tab("ai", { claudeSessionId: "sid-close" })];
    expect(buildSessionCloseRecord(tabs, "ai")).toEqual({
      claudeSessionId: "sid-close",
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

  it("fires terminal_session_record_close with reason 'explicit' (mock invoke)", async () => {
    mockInvoke.mockReset();
    mockInvoke.mockResolvedValueOnce({ success: true, message: null, data: null });
    const { invoke } = await import("@tauri-apps/api/core");
    const record = buildSessionCloseRecord([tab("ai", { claudeSessionId: "sid-close" })], "ai");
    expect(record).not.toBeNull();
    await invoke("terminal_session_record_close", record!);
    expect(mockInvoke).toHaveBeenCalledWith("terminal_session_record_close", {
      claudeSessionId: "sid-close",
      reason: "explicit",
    });
  });
});
