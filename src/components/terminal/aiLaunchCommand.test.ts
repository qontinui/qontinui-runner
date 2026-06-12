/** Launch-menu AI-session command builder (#548 Phase 1: pre-pin --session-id). */

import { describe, it, expect } from "vitest";
import { buildAiLaunchCommand } from "./aiLaunchCommand";

const CFG = "C:\\claude\\.claude-hotmail";

describe("buildAiLaunchCommand", () => {
  it("appends --session-id <fresh uuid> to the default Windows command", () => {
    const { command, pinnedSessionId } = buildAiLaunchCommand({
      configDir: CFG,
      isWindows: true,
      newSessionId: () => "11111111-1111-4111-8111-111111111111",
    });
    expect(pinnedSessionId).toBe("11111111-1111-4111-8111-111111111111");
    expect(command).toBe(
      `$env:CLAUDE_CONFIG_DIR="${CFG}"; claude --permission-mode bypassPermissions ` +
        `--session-id 11111111-1111-4111-8111-111111111111`,
    );
  });

  it("appends --session-id to the default POSIX command", () => {
    const { command } = buildAiLaunchCommand({
      configDir: "/h/.claude-x",
      isWindows: false,
      newSessionId: () => "abc",
    });
    expect(command).toBe(
      'CLAUDE_CONFIG_DIR="/h/.claude-x" claude --permission-mode bypassPermissions --session-id abc',
    );
  });

  it("never touches an operator-configured custom command (capture fallback)", () => {
    const { command, pinnedSessionId } = buildAiLaunchCommand({
      configDir: CFG,
      isWindows: true,
      customCmd: "clh",
      newSessionId: () => {
        throw new Error("must not mint an id for a custom command");
      },
    });
    expect(command).toBe("clh");
    expect(pinnedSessionId).toBeNull();
  });

  it("mints a FRESH id per call — ids are never reused across tabs/retries", () => {
    let n = 0;
    const opts = { configDir: CFG, isWindows: true, newSessionId: () => `uuid-${n++}` };
    expect(buildAiLaunchCommand(opts).pinnedSessionId).not.toBe(
      buildAiLaunchCommand(opts).pinnedSessionId,
    );
  });
});
