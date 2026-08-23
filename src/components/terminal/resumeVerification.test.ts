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
  detectResumeFailure,
  detectResumePicker,
  buildPickerAnswer,
  waitForClaudeHandshake,
  typeResumeAndVerify,
} from "./resumeVerification";
import { claudeDescriptor } from "./providerAdapter";
import { TERMINAL_EXITED, TERMINAL_WRITE_FAILED } from "./terminalWriteResult";

const CLAUDE_UI =
  "╭──────────────────────────────╮\n│ > │\n╰──────────────────────────────╯\n  ? for shortcuts";
const PLAIN_SHELL = "PS C:\\repo> claude --resume abc-123\r\nPS C:\\repo>";
// A bogus `--resume` that fell through: the CLI's unknown-session error is
// rendered INSIDE Claude-UI frames, so the positive handshake patterns match
// the same tail (the item-4 false-positive being fixed).
const BOGUS_RESUME_ERROR =
  "╭──────────────────────────────╮\n" +
  "│ No conversation found with session ID: fixture-ghost-0004 │\n" +
  "╰──────────────────────────────╯\n  ? for shortcuts";
const SESSION_PICKER =
  "Select a session to resume\n  1. fix the tests (2h ago)\n  2. refactor zone grid (1d ago)";

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

// Item 4 (boot-restore remediation): "the Claude TUI appeared" is not "the
// requested session resumed" — definitive failure frames must be recognized
// and must WIN over the positive handshake patterns.
describe("detectResumeFailure", () => {
  it.each([
    ["unknown-session error inside TUI frames", BOGUS_RESUME_ERROR],
    ["bare unknown-session error", "No conversation found with session ID: abc-123"],
    ["empty-history variant", "No conversations found"],
    ["interactive session picker", SESSION_PICKER],
    ["ANSI-wrapped error", `\x1b[31mNo conversation found\x1b[0m`],
  ])("recognizes a definitive resume failure: %s", (_name, text) => {
    expect(detectResumeFailure(text)).toBe(true);
  });

  it.each([
    ["healthy Claude UI", CLAUDE_UI],
    ["plain shell", PLAIN_SHELL],
    ["resume-size picker (a LANDED resume)", "  Resume from summary (recommended)"],
    ["empty buffer", ""],
  ])("does NOT flag non-failure output: %s", (_name, text) => {
    expect(detectResumeFailure(text)).toBe(false);
  });

  it("the bogus-resume tail ALSO matches the positive handshake (why negative-first matters)", () => {
    // Documents the false positive: without the negative check, this tail
    // verifies. The wait loop must therefore evaluate failure first.
    expect(detectClaudeHandshake(BOGUS_RESUME_ERROR)).toBe(true);
  });
});

// Phase 4 (provider-agnostic verification): when per-adapter HandshakePatterns
// are supplied, detection matches THOSE substrings (case-insensitive,
// ANSI-stripped) instead of the built-in Claude regex sets — so a non-Claude
// provider's resume verifies against its own banners.
describe("detectClaudeHandshake / detectResumeFailure (per-adapter patterns)", () => {
  const gemini = {
    success: ["gemini ready", "type your message"],
    failure: ["session not found", "no session to resume"],
  };

  it("matches the adapter's success substrings (and not Claude's)", () => {
    expect(detectClaudeHandshake("\x1b[32mGemini Ready\x1b[0m — type your message", gemini)).toBe(
      true,
    );
    // A Claude-only marker does NOT verify under the Gemini pattern set.
    expect(detectClaudeHandshake("? for shortcuts", gemini)).toBe(false);
  });

  it("matches the adapter's failure substrings (and not Claude's)", () => {
    expect(detectResumeFailure("Error: session not found", gemini)).toBe(true);
    // Claude's "No conversation found" is not a Gemini failure marker.
    expect(detectResumeFailure("No conversation found", gemini)).toBe(false);
  });

  it("an empty pattern list never matches (degrade safely, not false-positive)", () => {
    expect(detectClaudeHandshake("anything at all", { success: [], failure: [] })).toBe(false);
    expect(detectResumeFailure("anything at all", { success: [], failure: [] })).toBe(false);
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

  it("returns 'failed' when failure frames show — even though TUI frames are in the same tail", async () => {
    const readTail = vi.fn(async () => BOGUS_RESUME_ERROR);
    const out = await waitForClaudeHandshake("tab-1", {
      timeoutMs: 500,
      intervalMs: 1,
      readTail,
    });
    expect(out).toBe("failed");
  });

  it("returns 'failed' on the interactive session picker (resume fell through)", async () => {
    const readTail = vi.fn(async () => SESSION_PICKER);
    const out = await waitForClaudeHandshake("tab-1", {
      timeoutMs: 500,
      intervalMs: 1,
      readTail,
    });
    expect(out).toBe("failed");
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

  it("a bogus --resume (definitive failure frames) fails FAST — no pointless retype", async () => {
    const writes: string[] = [];
    const write = (_refs: never, _tab: string, text: string) => void writes.push(text);
    const out = await typeResumeAndVerify(new Map() as never, "tab-1", CMD, {
      write: write as never,
      settleMs: 1,
      timeoutMs: 50,
      intervalMs: 1,
      readTail: async () => BOGUS_RESUME_ERROR,
    });
    // The pane shows Claude UI frames, but the negative patterns win: the
    // requested session did NOT resume.
    expect(out).toBe("failed");
    // Definitive CLI answer ⇒ exactly ONE typed attempt (the retry exists
    // for lost keystrokes, not for a CLI that answered).
    expect(writes.filter((w) => w === CMD)).toHaveLength(1);
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

// ---------------------------------------------------------------------------
// Manual-test-loop iteration 10, item 2 — the resume WRITE RESULT was discarded.
//
// `writeWhenReady` dropped the `Promise<TerminalWriteResult>`, so a write
// refused with `TERMINAL_EXITED` looked exactly like one that landed and the
// loop spent its whole ~31 s budget (2 × 15 s poll) probing the scrollback of a
// pane whose process was already gone.
// ---------------------------------------------------------------------------
describe("typeResumeAndVerify — refused writes (item 2)", () => {
  const CMD_2 = "claude --resume abc-123\r";
  const exited = {
    success: false as const,
    code: TERMINAL_EXITED,
    error: "TERMINAL_EXITED: terminal tab-1 is not writable",
    hint: "Restart the session before writing to it.",
    terminalId: "tab-1",
    exitCode: 1,
  };
  const ipcFailed = {
    success: false as const,
    code: TERMINAL_WRITE_FAILED,
    error: "TERMINAL_WRITE_FAILED: terminal_write failed for tab-1",
    hint: "Check the runner log.",
    terminalId: "tab-1",
  };

  it("TERMINAL_EXITED fails fast — no scrollback probe, no retype, no 31s burn", async () => {
    let probes = 0;
    const writes: string[] = [];
    const onWriteFailure = vi.fn();
    const out = await typeResumeAndVerify(new Map() as never, "tab-1", CMD_2, {
      write: ((_r: never, _t: string, text: string) => {
        writes.push(text);
        return Promise.resolve(exited);
      }) as never,
      onWriteFailure,
      settleMs: 1,
      timeoutMs: 50,
      intervalMs: 1,
      readTail: async () => {
        probes += 1;
        return CLAUDE_UI;
      },
    });
    expect(out).toBe("failed");
    expect(probes).toBe(0);
    expect(writes.filter((w) => w === CMD_2)).toHaveLength(1);
    // The TYPED failure reaches the caller, not a generic "handshake not observed".
    expect(onWriteFailure).toHaveBeenCalledWith(exited);
  });

  it("TERMINAL_WRITE_FAILED skips this attempt's poll but still retypes once", async () => {
    let probes = 0;
    const writes: string[] = [];
    const out = await typeResumeAndVerify(new Map() as never, "tab-1", CMD_2, {
      write: ((_r: never, _t: string, text: string) => {
        writes.push(text);
        return Promise.resolve(ipcFailed);
      }) as never,
      settleMs: 1,
      timeoutMs: 50,
      intervalMs: 1,
      readTail: async () => {
        probes += 1;
        return CLAUDE_UI;
      },
    });
    expect(out).toBe("failed");
    expect(probes).toBe(0);
    expect(writes.filter((w) => w === CMD_2)).toHaveLength(2);
  });

  it("an OK envelope behaves exactly as before — the happy path is untouched", async () => {
    const out = await typeResumeAndVerify(new Map() as never, "tab-1", CMD_2, {
      write: (() => Promise.resolve({ success: true, bytes: 4 })) as never,
      settleMs: 1,
      timeoutMs: 50,
      intervalMs: 1,
      readTail: async () => CLAUDE_UI,
    });
    expect(out).toBe("verified");
  });

  it("a writer that resolves NOTHING is 'no information', not a failure", async () => {
    const out = await typeResumeAndVerify(new Map() as never, "tab-1", CMD_2, {
      write: (() => undefined) as never,
      settleMs: 1,
      timeoutMs: 50,
      intervalMs: 1,
      readTail: async () => CLAUDE_UI,
    });
    expect(out).toBe("verified");
  });
});

// ---------------------------------------------------------------------------
// Manual-test-loop iteration 10, item 3 — the handshake REGEX SET was dead code.
//
// Boot-restore ALWAYS passes the provider descriptor's `HandshakePatterns`, and
// the detectors short-circuited on it (`if (patterns) return substringsOnly`).
// The box-frame marker `/[╭╰]─{3,}/` — which no substring can express — was
// therefore never consulted on the only path that runs in production.
// ---------------------------------------------------------------------------
describe("descriptor-driven detection unions the regexes (item 3)", () => {
  const patterns = claudeDescriptor.handshakePatterns();
  /** A restored pane that has painted ONLY the rounded input box. */
  const FRAME_ONLY =
    "╭────────────────────────────╮\n│ >                          │\n╰────────────────────────────╯";

  it("a pane showing only the box frame VERIFIES through the live (descriptor) path", () => {
    expect(detectClaudeHandshake(FRAME_ONLY, patterns)).toBe(true);
  });

  it("the frame marker is genuinely regex-only — no success substring matches it", () => {
    const lower = FRAME_ONLY.toLowerCase();
    expect(patterns.success.some((sub) => lower.includes(sub.toLowerCase()))).toBe(false);
  });

  it("the substring half still matches — the union added to it, it did not replace it", () => {
    expect(detectClaudeHandshake("  ? for shortcuts", patterns)).toBe(true);
  });

  it("a plain shell still does NOT verify — the union did not weaken the gate", () => {
    expect(detectClaudeHandshake(PLAIN_SHELL, patterns)).toBe(false);
  });

  it("failure detection unions its regexes too, and still wins over the frames", () => {
    expect(detectResumeFailure(BOGUS_RESUME_ERROR, patterns)).toBe(true);
    expect(detectResumeFailure(SESSION_PICKER, patterns)).toBe(true);
    expect(detectResumeFailure(FRAME_ONLY, patterns)).toBe(false);
  });

  it("a descriptor with NO regexes matches substrings only — no Claude leakage", () => {
    const gemini = { success: ["gemini ready"], failure: ["no such chat"] };
    expect(detectClaudeHandshake(FRAME_ONLY, gemini)).toBe(false);
    expect(detectClaudeHandshake("Gemini ready", gemini)).toBe(true);
  });
});
