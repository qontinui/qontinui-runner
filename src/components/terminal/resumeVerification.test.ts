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
  // Default pane probe: a plain shell with no claude, so the typing paths run.
  invoke: vi.fn(async (cmd: string) =>
    cmd === "terminal_probe_claude" ? { data: { state: "absent", sessionIds: [] } } : undefined,
  ),
}));

import {
  stripAnsi,
  renderAnsi,
  lastOscTitle,
  detectClaudeHandshake,
  detectResumeFailure,
  detectResumePicker,
  buildPickerAnswer,
  waitForClaudeHandshake,
  typeResumeAndVerify,
  probeClaudeInPane,
  clearLineSequence,
} from "./resumeVerification";
import { claudeDescriptor } from "./providerAdapter";
import { buildResumeCmd } from "./useTerminalInitialization";
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

  it("collapses cursor-addressed text, so phrases a conversation mentions do not match", () => {
    expect(stripAnsi("No\x1b[1Cconversation\x1b[1Cfound")).toBe("Noconversationfound");
  });
});

describe("renderAnsi", () => {
  it("renders cursor motion: forward → spaces, vertical/position → newline", () => {
    expect(renderAnsi("? for\x1b[1Cshortcuts")).toBe("? for shortcuts");
    expect(renderAnsi("? for\x1b[Cshortcuts")).toBe("? for shortcuts");
    expect(renderAnsi("bypass\x1b[3Cpermissions")).toBe("bypass   permissions");
    expect(renderAnsi("a\x1b[7Gb")).toBe("a b");
    expect(renderAnsi("rule\r\x1b[1B\u276f")).toBe("rule\r\n\u276f");
    expect(renderAnsi("x\x1b[9;1Hy")).toBe("x\ny");
    expect(renderAnsi("abc\x1b[5ddef")).toBe("abc\ndef");
    expect(renderAnsi("abc\x1b[2Adef")).toBe("abc\ndef");
  });

  it("drops private-parameter CSI, charset designations, two-byte escapes and SI/SO", () => {
    expect(renderAnsi("\x1b[>0q\x1b[?u\x1b[>4mok\x1b(B\x0f\x1b7\x1b8\x1bM\x1bc")).toBe("ok");
  });

  it("drops OSC strings terminated by BEL or ST, and DCS strings", () => {
    expect(renderAnsi("\x1b]0;title\x07a\x1b]633;E;cmd\x1b\\b\x1bPq#0\x1b\\c")).toBe("abc");
  });

  it("keeps the visible text of an OSC 8 hyperlink", () => {
    expect(renderAnsi("\x1b]8;;https://x\x07link\x1b]8;;\x07")).toBe("link");
  });
});

describe("lastOscTitle", () => {
  it("returns the most recent OSC 0/2 title, ignoring other OSC marks", () => {
    expect(lastOscTitle("\x1b]0;\u2733 Claude Code\x07\x1b]633;E;claude\x07\x1b]2;two\x1b\\")).toBe(
      "two",
    );
    expect(lastOscTitle("no titles here")).toBeUndefined();
  });
});

// Claude Code v2 (2.1.286) first paint, captured 2026-10-01 from a boot-restored
// pane whose resume verification timed out and was retyped into the live
// session. Byte-verbatim apart from the shortened cwd and session title. The
// shell half carries the typed command in an OSC 633;E mark and an OSC 0 shell
// title, neither of which may verify on its own.
const V2_SESSION_ID = "230feb99-2dd7-42d7-92bc-6d36c1883089";
// The exact line the restore types (env thresholds included), echoed by the
// shell and repeated in the shell-integration OSC 633;E mark.
const V2_TYPED = buildResumeCmd(V2_SESSION_ID, "/home/user/.claude", "full").replace(/\r$/, "");
const V2_SHELL_ECHO =
  "\x1b]633;A\x07\x1b]0;user@host: ~/repo\x07\x1b[01;32muser@host\x1b[00m:\x1b[01;34m~/repo\x1b[00m$ \x1b]633;B\x07" +
  `${V2_TYPED}\r\n` +
  `\x1b[?2004l\r\x1b]633;E;${V2_TYPED}\x07\x1b]633;C\x07`;
const V2_FIRST_PAINT =
  "\x1b7\x1b[r\x1b8\x1b[?25h\x1b[?25l\x1b[?2004h\x1b[?2031h\x1b[?1004h\x1b[>0q\x1b[?u\x1b[c\x1b[>4m" +
  "\x1b]0;\u2733 session title\x07\x1b[?1049h\x1b[2J\x1b[H\x1b[?1000h\x1b[?25l\x1b[H\x1b[7Gby\x1b[10Gits\x1b[14Gid.\r" +
  "\x1b[2B\x1b[38;5;246m\u273b\x1b[3GChurned for 1m 31s \u00b7 done 7:49 AM\r\x1b[28C\x1b[1B\u25d0 medium \u00b7 /effort\r" +
  "\x1b[1B\x1b[38;5;244m" +
  "\u2500".repeat(48) +
  "\r\x1b[1B\x1b[39m\u276f\u00a0\r\x1b[1B\x1b[38;5;244m" +
  "\u2500".repeat(48) +
  "\x1b[39m";

describe("Claude Code v2 handshake (no rounded box, cursor-addressed text)", () => {
  const hp = claudeDescriptor.handshakePatterns();

  it("verifies the real v2 first paint of a resumed session", () => {
    expect(detectClaudeHandshake(V2_SHELL_ECHO + V2_FIRST_PAINT, hp)).toBe(true);
    expect(detectResumeFailure(V2_SHELL_ECHO + V2_FIRST_PAINT, hp)).toBe(false);
  });

  it("the shell half alone does not verify (command echo, OSC 633 marks, shell title)", () => {
    expect(detectClaudeHandshake(V2_SHELL_ECHO, hp)).toBe(false);
  });

  it("the typed command includes the env thresholds the restore really sends", () => {
    expect(V2_TYPED).toContain("CLAUDE_CODE_RESUME_TOKEN_THRESHOLD");
    expect(V2_TYPED).toContain(`--resume ${V2_SESSION_ID}`);
  });

  it("recognizes the v2 logo line, which carries the version", () => {
    const logo =
      "\x1b[38;5;174m \u2590\u259b\u2588\u2588\u2588\u259c\u258c\x1b[39m\x1b[2CClaude Code\x1b[1Cv2.1.286";
    expect(detectClaudeHandshake(logo, hp)).toBe(true);
  });

  it("recognizes the 'Claude Code' window title Claude sets at launch", () => {
    expect(detectClaudeHandshake("\x1b]0;\u2733 Claude Code\x07", hp)).toBe(true);
  });

  it.each([
    ["a bare ❯ shell prompt", "~/repo on main\r\n\u276f "],
    ["a bare rule", "\u2500".repeat(40)],
    [
      "a two-line prompt whose first line ends in a ─ fill",
      "~/repo \x1b[2m" + "\u2500".repeat(60) + "\x1b[0m\r\n\x1b[1;32m\u276f\x1b[0m ",
    ],
    [
      "a CLI error printed to the shell",
      "$ claude --resume x\r\nClaude Code requires Node.js version 22 or higher.\r\n$ ",
    ],
    ["the folder-trust dialog", "Claude\x1b[9GCode'll be able to read files in this folder"],
    [
      "a title an earlier Claude set, after it exited",
      "\x1b]0;\u2733 Claude Code\x07bye\x1b]0;\x07\r\n$ ",
    ],
    [
      "a shell title whose path contains Claude Code",
      "\x1b]0;user@host: ~/Projects/Claude Code demo\x07$ ",
    ],
    ["a shell title that is a bare ~/Claude Code path", "\x1b]2;~/Claude Code\x07$ "],
    ["shell output listing a directory named Claude Code", "$ ls -1\r\nClaude Code\r\nnotes\r\n$ "],
  ])("does NOT verify: %s", (_name, text) => {
    expect(detectClaudeHandshake(text, hp)).toBe(false);
  });

  it.each([
    ["the unknown-session error", "No\x1b[1Cconversation\x1b[1Cfound\x1b[1Cwith\x1b[1Csession"],
    ["the session-picker title", "Select\x1b[1Ca\x1b[1Csession\x1b[1Cto\x1b[1Cresume"],
  ])("a resumed conversation that mentions %s is not a failure", (_name, mention) => {
    const live = V2_SHELL_ECHO + V2_FIRST_PAINT + "\r\n" + mention;
    expect(detectResumeFailure(live, hp)).toBe(false);
    expect(detectClaudeHandshake(live, hp)).toBe(true);
  });

  it("picker wording in a resumed conversation does not trigger the picker answer", () => {
    const live = V2_FIRST_PAINT + "\r\nResume\x1b[1Cfull\x1b[1Csession\x1b[1Cas-is";
    expect(detectResumePicker(live)).toBe(false);
  });

  it("a long run of ─ with no prompt is rejected", () => {
    // The frame regex is anchored to line starts, so this is linear. No
    // wall-clock assertion: it would flake on a loaded CI runner.
    expect(detectClaudeHandshake("\u2500".repeat(16_000), hp)).toBe(false);
  });

  it("a failure frame still wins over the v2 markers", () => {
    const failed = "\x1b]0;\u2733 Claude Code\x07No conversation found with session ID: x";
    expect(detectResumeFailure(failed, hp)).toBe(true);
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
    // attempt 1: CMD; attempt 2: Ctrl-E Ctrl-U (clear any half-typed line; ESC on Windows) + CMD.
    expect(writes).toEqual([CMD, "\x05\x15", CMD]);
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

describe("typeResumeAndVerify (does not type into a live claude)", () => {
  const SID = "5c46c390-037d-4b8e-9d7a-1f2e3d4c5b6a";
  const CMD = `claude --permission-mode bypassPermissions --resume ${SID}\r`;
  const base = { settleMs: 1, timeoutMs: 10, intervalMs: 1, sessionId: SID };
  const recorder = () => {
    const writes: string[] = [];
    const write = (_refs: never, _tab: string, text: string) => void writes.push(text);
    return { writes, write: write as never };
  };

  it("operator retry against a pane already running THIS session: types nothing, verifies", async () => {
    // Case-insensitive: the command line's spelling of the id is what it is.
    const { writes, write } = recorder();
    const out = await typeResumeAndVerify(new Map() as never, "tab-1", CMD, {
      ...base,
      write,
      // A live session whose frame the scrape does not recognise.
      readTail: async () => "some redraw the patterns miss",
      probeClaude: async () => ({ state: "live", sessionIds: [SID.toUpperCase()] }),
    });
    expect(out).toBe("verified");
    expect(writes).toEqual([]);
  });

  it("a claude running some OTHER (or unparseable) session: types nothing, fails", async () => {
    for (const sessionIds of [["00000000-0000-4000-8000-000000000000"], []]) {
      const { writes, write } = recorder();
      const out = await typeResumeAndVerify(new Map() as never, "tab-1", CMD, {
        ...base,
        write,
        readTail: async () => "",
        probeClaude: async () => ({ state: "live", sessionIds }),
      });
      expect(out).toBe("failed");
      expect(writes).toEqual([]);
    }
  });

  it("skips the retype (and its ESC) once the first attempt brought claude up unseen by the scrape", async () => {
    const { writes, write } = recorder();
    const out = await typeResumeAndVerify(new Map() as never, "tab-1", CMD, {
      ...base,
      write,
      readTail: async () => "",
      probeClaude: async () =>
        writes.includes(CMD)
          ? { state: "live", sessionIds: [SID] }
          : { state: "absent", sessionIds: [] },
    });
    expect(out).toBe("verified");
    expect(writes).toEqual([CMD]);
  });

  it("an UNREADABLE process table fails closed — nothing typed", async () => {
    const { writes, write } = recorder();
    const out = await typeResumeAndVerify(new Map() as never, "tab-1", CMD, {
      ...base,
      write,
      readTail: async () => "",
      probeClaude: async () => ({ state: "unknown", sessionIds: [] }),
    });
    expect(out).toBe("failed");
    expect(writes).toEqual([]);
  });

  it("a REMOTE pane (unprobeable) types exactly as before", async () => {
    const { writes, write } = recorder();
    const out = await typeResumeAndVerify(new Map() as never, "tab-1", CMD, {
      ...base,
      write,
      readTail: async () => "$ ",
      probeClaude: async () => ({ state: "remote", sessionIds: [] }),
    });
    expect(out).toBe("failed");
    expect(writes).toEqual([CMD, "\x05\x15", CMD]);
  });

  it("skipFirstProbe (fresh boot-restore pane) probes only before the retype", async () => {
    const { write } = recorder();
    let probes = 0;
    await typeResumeAndVerify(new Map() as never, "tab-1", CMD, {
      ...base,
      write,
      skipFirstProbe: true,
      readTail: async () => "$ ",
      probeClaude: async () => {
        probes++;
        return { state: "absent", sessionIds: [] };
      },
    });
    expect(probes).toBe(1);
  });
});

describe("probeClaudeInPane", () => {
  it("passes readings through and maps anything else to unknown", async () => {
    expect(
      await probeClaudeInPane("t", async () => ({ data: { state: "live", sessionIds: ["a", 3] } })),
    ).toEqual({ state: "live", sessionIds: ["a"] });
    expect(await probeClaudeInPane("t", async () => ({ data: { state: "absent" } }))).toEqual({
      state: "absent",
      sessionIds: [],
    });
    expect(await probeClaudeInPane("t", async () => ({ data: { state: "remote" } }))).toEqual({
      state: "remote",
      sessionIds: [],
    });
    const unknown = { state: "unknown", sessionIds: [] };
    expect(await probeClaudeInPane("t", async () => ({ data: { state: "unknown" } }))).toEqual(
      unknown,
    );
    expect(await probeClaudeInPane("t", async () => undefined)).toEqual(unknown);
    expect(
      await probeClaudeInPane("t", async () => {
        throw new Error("Terminal not found");
      }),
    ).toEqual(unknown);
  });
});

// 2026-10-08 — the retry's line-clear was a bare ESC, which GNU readline/zsh
// read as the META prefix: it combined with the retyped command's first key
// (`C` → M-C, capitalize-word) and the retype ran as `LAUDE_CODE_…`. Only
// PSReadLine treats a bare ESC as "clear line".
describe("clearLineSequence (retry line-clear per host OS)", () => {
  it("never sends a bare ESC to a readline/zsh shell", () => {
    const seq = clearLineSequence(false);
    expect(seq).not.toContain("\x1b");
    expect(seq).toBe("\x05\x15");
  });
  it("keeps ESC (RevertLine) for PSReadLine on Windows", () => {
    expect(clearLineSequence(true)).toBe("\x1b");
  });
});
