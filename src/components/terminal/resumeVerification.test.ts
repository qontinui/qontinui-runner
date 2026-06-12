/**
 * Tests for the resume-landed verification loop (Phase 3 of
 * `2026-06-12-runner-session-registry-and-restore-hardening`, issue #548):
 * handshake detection over decoded PTY output, the poll-until-deadline wait,
 * and the type → verify → retry-once state machine.
 *
 * vitest runs `environment: "node"` with no React Testing Library; the
 * scrollback probe and writer are injectable so no IPC is exercised (same
 * precedent as `useTerminalInitialization.test.ts`).
 */

import { describe, it, expect, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

import {
  stripAnsi,
  detectClaudeHandshake,
  detectResumePicker,
  buildPickerAnswer,
  waitForClaudeHandshake,
  typeResumeAndVerify,
} from "./resumeVerification";

const CLAUDE_UI =
  "╭──────────────────────────────╮\n│ > │\n╰──────────────────────────────╯\n  ? for shortcuts";
const PLAIN_SHELL = "PS C:\\repo> claude --resume abc-123\r\nPS C:\\repo>";

describe("stripAnsi", () => {
  it("removes CSI sequences so patterns match rendered text", () => {
    expect(stripAnsi("\x1b[38;5;205m? for shortcuts\x1b[0m")).toBe("? for shortcuts");
  });
});

describe("detectClaudeHandshake", () => {
  it.each([
    ["status-line shortcuts hint", "  ? for shortcuts"],
    ["working indicator", "✻ Pondering… (esc to interrupt)"],
    ["bypass-permissions status line", "  bypass permissions on"],
    ["welcome banner", "Welcome back to Claude Code!"],
    ["rounded input-box frame", CLAUDE_UI],
    ["ANSI-wrapped UI", `\x1b[2m${CLAUDE_UI}\x1b[0m`],
  ])("recognizes the Claude UI: %s", (_name, text) => {
    expect(detectClaudeHandshake(text)).toBe(true);
  });

  it.each([
    ["bare shell prompt + command echo", PLAIN_SHELL],
    ["command-not-found error", "claude : The term 'claude' is not recognized"],
    ["unrelated shell output", "Directory: D:\\repo\r\nMode  LastWriteTime  Name"],
    ["empty buffer", ""],
  ])("does NOT count non-Claude output: %s", (_name, text) => {
    // The verification must never pass on a pane that is still a bare shell —
    // that false positive is exactly the silent failure mode being fixed.
    expect(detectClaudeHandshake(text)).toBe(false);
  });

  it("counts the resume-size picker as a landed resume (it IS Claude UI)", () => {
    const picker =
      "╭──────────────────────────────╮\n" +
      "  Resume from summary (recommended)\n  Resume full session as-is\n  Don't ask me again";
    expect(detectClaudeHandshake(picker)).toBe(true);
  });
});

describe("detectResumePicker / buildPickerAnswer", () => {
  const PICKER =
    "  Resume from summary (recommended)\n  Resume full session as-is\n  Don't ask me again";

  it("recognizes the CLI's resume-size picker (ANSI-wrapped too)", () => {
    expect(detectResumePicker(PICKER)).toBe(true);
    expect(detectResumePicker(`\x1b[36m${PICKER}\x1b[0m`)).toBe(true);
  });

  it("does NOT match ordinary Claude UI or shell output", () => {
    expect(detectResumePicker(CLAUDE_UI)).toBe(false);
    expect(detectResumePicker(PLAIN_SHELL)).toBe(false);
    expect(detectResumePicker("")).toBe(false);
  });

  it("answers option 2 (full as-is) for the default policy and 1 for summary", () => {
    expect(buildPickerAnswer("full")).toBe("2\r");
    expect(buildPickerAnswer("summary")).toBe("1\r");
  });
});

describe("waitForClaudeHandshake", () => {
  it("verifies as soon as a probe shows the Claude UI", async () => {
    const tails = [PLAIN_SHELL, PLAIN_SHELL, CLAUDE_UI];
    const readTail = vi.fn(async () => tails.shift() ?? CLAUDE_UI);
    const out = await waitForClaudeHandshake("tab-1", {
      timeoutMs: 500,
      intervalMs: 1,
      readTail,
    });
    expect(out).toBe("verified");
    expect(readTail).toHaveBeenCalledTimes(3);
  });

  it("times out when the pane never shows the Claude UI", async () => {
    const readTail = vi.fn(async () => PLAIN_SHELL);
    const out = await waitForClaudeHandshake("tab-1", {
      timeoutMs: 10,
      intervalMs: 1,
      readTail,
    });
    expect(out).toBe("timeout");
  });

  it("treats an unreadable scrollback (null) as no-evidence, not success", async () => {
    const readTail = vi.fn(async () => null);
    const out = await waitForClaudeHandshake("tab-1", {
      timeoutMs: 10,
      intervalMs: 1,
      readTail,
    });
    expect(out).toBe("timeout");
  });
});

describe("typeResumeAndVerify (retry-once state machine)", () => {
  const CMD = "claude --resume abc-123\r";

  it("types once and verifies when the handshake appears", async () => {
    const writes: string[] = [];
    const write = (_refs: never, _tab: string, text: string) => void writes.push(text);
    const out = await typeResumeAndVerify(new Map() as never, "tab-1", CMD, {
      write: write as never,
      settleMs: 1,
      timeoutMs: 50,
      intervalMs: 1,
      readTail: async () => CLAUDE_UI,
    });
    expect(out).toBe("verified");
    expect(writes).toEqual([CMD]);
  });

  it("retries ONCE (clear-line then retype the same command) when the first attempt never lands", async () => {
    const writes: string[] = [];
    const write = (_refs: never, _tab: string, text: string) => void writes.push(text);
    // First attempt: pane stays a bare shell. Second attempt (the retype):
    // Claude UI shows — keyed off the write log so the flip is deterministic
    // regardless of probe pacing.
    const readTail = async () =>
      writes.filter((w) => w === CMD).length >= 2 ? CLAUDE_UI : PLAIN_SHELL;
    const out = await typeResumeAndVerify(new Map() as never, "tab-1", CMD, {
      write: write as never,
      settleMs: 1,
      timeoutMs: 10,
      intervalMs: 1,
      readTail,
    });
    expect(out).toBe("verified");
    // attempt 1: CMD; attempt 2: ESC (clear any half-typed line) + CMD.
    expect(writes).toEqual([CMD, "\x1b", CMD]);
  });

  it("answers the resume-size picker at most ONCE when pickerAnswer is set", async () => {
    const PICKER =
      "╭──────────────╮\n  ❯ Resume from summary (recommended)\n    Resume full session as-is\n    Don't ask me again";
    const writes: string[] = [];
    const write = (_refs: never, _tab: string, text: string) => void writes.push(text);
    const out = await typeResumeAndVerify(new Map() as never, "tab-1", CMD, {
      write: write as never,
      settleMs: 1,
      timeoutMs: 50,
      intervalMs: 1,
      pickerAnswer: "2\r",
      readTail: async () => PICKER,
    });
    // The picker IS Claude UI — the resume landed, so this verifies...
    expect(out).toBe("verified");
    // ...and the configured answer was typed exactly once.
    expect(writes.filter((w) => w === "2\r")).toHaveLength(1);
  });

  it("never types a picker answer when the picker is absent", async () => {
    const writes: string[] = [];
    const write = (_refs: never, _tab: string, text: string) => void writes.push(text);
    await typeResumeAndVerify(new Map() as never, "tab-1", CMD, {
      write: write as never,
      settleMs: 1,
      timeoutMs: 50,
      intervalMs: 1,
      pickerAnswer: "2\r",
      readTail: async () => CLAUDE_UI,
    });
    expect(writes).not.toContain("2\r");
  });

  it("fails after the retry is exhausted and never falsely verifies", async () => {
    const writes: string[] = [];
    const write = (_refs: never, _tab: string, text: string) => void writes.push(text);
    const out = await typeResumeAndVerify(new Map() as never, "tab-1", CMD, {
      write: write as never,
      settleMs: 1,
      timeoutMs: 5,
      intervalMs: 1,
      readTail: async () => PLAIN_SHELL,
    });
    expect(out).toBe("failed");
    // Exactly two typed attempts — the spec is retry ONCE, not retry forever.
    expect(writes.filter((w) => w === CMD)).toHaveLength(2);
  });
});
