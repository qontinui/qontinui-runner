/**
 * Thin-wrapper test for `buildAiLaunchCommand` (#548/#779 seam, Phase 3).
 *
 * The flag-composition logic (default-template append, `{sessionId}`
 * substitution, blank→built-in fallback, per-account verbatim precedence) now
 * lives in Rust (`claude_session/launch_spec.rs`) and is covered by its unit
 * tests. Here we only assert the wrapper invokes the tauri command with the
 * passed args and maps the `{ command, pinnedSessionId }` response through.
 */

import { describe, it, expect, vi, beforeEach } from "vitest";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

import { buildAiLaunchCommand, buildAiLaunchCommandForTab } from "./aiLaunchCommand";

describe("buildAiLaunchCommand (thin tauri wrapper)", () => {
  beforeEach(() => invokeMock.mockReset());

  it("invokes build_ai_launch_command with the passed args and returns the pinned result", async () => {
    invokeMock.mockResolvedValue({
      success: true,
      data: {
        command: 'CLAUDE_CONFIG_DIR="/h/.claude-x" claude --permission-mode bypassPermissions --session-id abc',
        pinnedSessionId: "abc",
      },
    });

    const result = await buildAiLaunchCommand({
      configDir: "/h/.claude-x",
      isWindows: false,
      sessionId: "abc",
    });

    expect(invokeMock).toHaveBeenCalledWith("build_ai_launch_command", {
      configDir: "/h/.claude-x",
      sessionId: "abc",
      isWindows: false,
    });
    expect(result).toEqual({
      command: 'CLAUDE_CONFIG_DIR="/h/.claude-x" claude --permission-mode bypassPermissions --session-id abc',
      pinnedSessionId: "abc",
    });
  });

  it("maps a null pinnedSessionId (opaque per-account alias, verbatim) through", async () => {
    invokeMock.mockResolvedValue({
      success: true,
      data: { command: "clh", pinnedSessionId: null },
    });

    const result = await buildAiLaunchCommand({
      configDir: "C:\\claude\\.claude-hotmail",
      isWindows: true,
      sessionId: "abc",
    });

    expect(result).toEqual({ command: "clh", pinnedSessionId: null });
  });

  it("throws when the command returns no data", async () => {
    invokeMock.mockResolvedValue({ success: false, message: "boom" });
    await expect(
      buildAiLaunchCommand({ configDir: "/x", isWindows: false, sessionId: "abc" }),
    ).rejects.toThrow("boom");
  });
});

/**
 * The orphan-cleanup contract (prompts-panel manual-test remediation, item 6).
 *
 * `handleLaunchAiSession` creates the PTY BEFORE it builds the launch spec, so
 * a throw in between left a bare shell open with no toast and no log. The
 * wrapper must close that tab, report the reason, and answer null.
 */
describe("buildAiLaunchCommandForTab (orphan cleanup on a failed build)", () => {
  beforeEach(() => invokeMock.mockReset());

  const params = { configDir: "C:/claude/.claude-work", isWindows: true, sessionId: "abc" };

  it("returns the built command and touches neither handler on success", async () => {
    invokeMock.mockResolvedValueOnce({
      success: true,
      data: { command: "claude --session-id abc", pinnedSessionId: "abc" },
    });
    const disposeTab = vi.fn();
    const notify = vi.fn();

    const result = await buildAiLaunchCommandForTab("tab-1", params, { disposeTab, notify });

    expect(result).toEqual({ command: "claude --session-id abc", pinnedSessionId: "abc" });
    expect(disposeTab).not.toHaveBeenCalled();
    expect(notify).not.toHaveBeenCalled();
  });

  it("disposes the orphaned tab, surfaces the reason and answers null when the build throws", async () => {
    // `…Once`, the idiom every other invoke-rejection test here uses: a
    // PERSISTENT rejecting mock outlives the call under test and vitest reports
    // the leftover rejection as an unhandled error, failing an otherwise green
    // test.
    invokeMock.mockRejectedValueOnce(new Error("launch spec unreadable"));
    const disposeTab = vi.fn();
    const notify = vi.fn();
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});

    const result = await buildAiLaunchCommandForTab("tab-1", params, { disposeTab, notify });

    expect(result).toBeNull();
    expect(disposeTab).toHaveBeenCalledWith("tab-1");
    // The operator must be told WHICH account failed and WHY — a bare "launch
    // failed" is what made this path expensive to diagnose.
    const [message] = notify.mock.calls[0] as [string];
    expect(message).toContain("C:/claude/.claude-work");
    expect(message).toContain("launch spec unreadable");
    expect(consoleError).toHaveBeenCalled();

    consoleError.mockRestore();
  });

  it("reports a data-less response (a non-Error rejection stringifies) and still disposes", async () => {
    // The `data`-less branch of `buildAiLaunchCommand` — the shape a failed
    // Rust command actually returns.
    invokeMock.mockResolvedValueOnce({ success: false, message: "no account configured" });
    const disposeTab = vi.fn();
    const notify = vi.fn();
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});

    const result = await buildAiLaunchCommandForTab("tab-2", params, { disposeTab, notify });

    expect(result).toBeNull();
    expect(disposeTab).toHaveBeenCalledWith("tab-2");
    expect((notify.mock.calls[0] as [string])[0]).toContain("no account configured");

    consoleError.mockRestore();
  });

  it("stringifies a rejection that is not an Error", async () => {
    // A bare string rejection — what a Tauri IPC fault can surface as.
    invokeMock.mockRejectedValueOnce("plain string fault");
    const disposeTab = vi.fn();
    const notify = vi.fn();
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});

    await buildAiLaunchCommandForTab("tab-3", params, { disposeTab, notify });

    expect((notify.mock.calls[0] as [string])[0]).toContain("plain string fault");
    expect(disposeTab).toHaveBeenCalledWith("tab-3");

    consoleError.mockRestore();
  });
});
