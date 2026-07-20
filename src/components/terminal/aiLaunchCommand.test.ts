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

import { buildAiLaunchCommand } from "./aiLaunchCommand";

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
