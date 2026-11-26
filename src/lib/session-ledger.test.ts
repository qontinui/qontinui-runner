/**
 * Shape tests for the hand-written session-ledger types.
 *
 * Each sample below is a FULLY-specified value of its type, so `tsc` rejects
 * a missing or an extra field; the runtime half compares its keys with the
 * golden `src-tauri/src/session/session_ledger.rs` is also tested against
 * (`serialized_keys_match_the_typescript_golden`). Together they pin the TS
 * types to the serde output.
 */

import { describe, it, expect, vi, beforeEach } from "vitest";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

import golden from "./__golden__/session-ledger-keys.json";
import {
  captureSessionLedger,
  displayNameOf,
  getSessionLedgerReport,
  setSessionFinished,
  type LedgerCapture,
  type LedgerEntry,
  type LedgerGeneration,
  type LedgerOutcome,
  type LedgerReport,
  type SessionLedger,
} from "./session-ledger";

const entry: LedgerEntry = {
  claudeSessionId: "aaaa-1111",
  terminalId: "term-1",
  pageId: "default",
  zoneIndex: 0,
  title: "claude",
  sessionName: "amber-otter",
  nameSource: null,
  accountLabel: "gmail",
  configDir: "C:/claude/.claude-gmail",
  resumeConfigDir: "C:/claude/.claude-gmail",
  resumeCommand:
    'cd "D:/repo" && CLAUDE_CONFIG_DIR="C:/claude/.claude-gmail" claude --resume aaaa-1111',
  provider: "claude",
  workingDir: "D:/repo/sub",
  lastSeenAt: 1_700_000_000_000,
  finished: false,
  restorable: true,
  worktreePath: "D:/repo",
  planSlug: "2026-10-04-runner-session-roster-restore-picker",
  workUnitId: null,
  wipState: "captured",
  wipRef: "refs/wip/aaaa-1111",
  custodySessionMismatch: false,
};

const ledger: SessionLedger = {
  ledgerVersion: 1,
  capturedAtMs: 1_700_000_000_000,
  capturedAt: "2023-11-14T22:13:20+00:00",
  reason: "poll",
  bootAtMs: 1_699_999_000_000,
  shutdownAt: null,
  cleanShutdown: true,
  sessions: [entry],
};

const outcome: LedgerOutcome = {
  claudeSessionId: "aaaa-1111",
  terminalId: "term-1",
  pageId: "default",
  zoneIndex: 0,
  displayName: "amber-otter",
  sessionName: "amber-otter",
  nameSource: null,
  title: "claude",
  accountLabel: "gmail",
  configDir: "C:/claude/.claude-gmail",
  configDirKnown: true,
  provider: "claude",
  workingDir: "D:/repo/sub",
  worktreePath: "D:/repo",
  planSlug: null,
  workUnitId: null,
  wipState: null,
  wipRef: null,
  custodySessionMismatch: false,
  lastSeenAt: 1_700_000_000_000,
  restorable: true,
  finished: false,
  outcome: "missing",
  reason: "no-attempt",
  resumeDir: "D:/repo/sub",
  resumeCommand:
    'cd "D:/repo/sub" && CLAUDE_CONFIG_DIR="C:/claude/.claude-gmail" claude --resume aaaa-1111',
  resumeAccount: { known: true, configDir: "C:/claude/.claude-gmail" },
};

const generation: LedgerGeneration = {
  file: "session-ledger.1700000000000.json",
  rotatedAtMs: 1_700_000_000_000,
  bootAtMs: 1_699_999_000_000,
  thisBoot: true,
  capturedAtMs: 1_699_999_900_000,
  capturedAt: "2023-11-14T21:58:20+00:00",
  reason: "poll",
  cleanShutdown: true,
  sessionCount: 1,
  verdict: "mismatch",
  returned: [],
  missing: [outcome],
  finished: [],
  closedByUser: [],
};

const report: LedgerReport = {
  status: "ok",
  reason: null,
  generatedAt: 1_700_000_100_000,
  priorCapturedAt: "2023-11-14T21:58:20+00:00",
  priorReason: "poll",
  expected: [entry],
  returned: [],
  missing: [outcome],
  finished: [],
  closedByUser: [],
  verdict: "mismatch",
  current: ledger,
  generations: [generation],
  savedAtMs: 1_700_000_050_000,
  savedMatchesCurrent: true,
  note: "0 of 1 sessions open before the last shutdown came back.",
};

const capture: LedgerCapture = { persisted: true, path: "x", ledger };

const sortedKeys = (o: object): string[] => Object.keys(o).sort();
const goldenKeys = (ty: keyof typeof golden): string[] => [...(golden[ty] as string[])].sort();

describe("session-ledger types match the Rust serde golden", () => {
  it.each([
    ["LedgerEntry", entry],
    ["SessionLedger", ledger],
    ["LedgerOutcome", outcome],
    ["ResumeAccount", outcome.resumeAccount],
    ["LedgerGeneration", generation],
    ["LedgerReport", report],
    ["LedgerCapture", capture],
  ] as const)("%s", (ty, sample) => {
    expect(sortedKeys(sample)).toEqual(goldenKeys(ty));
  });
});

describe("displayNameOf mirrors the Rust display-name rule", () => {
  const id = "0123456789abcdef";
  const of = (sessionName: string | null, nameSource: string | null, title: string | null) =>
    displayNameOf({ claudeSessionId: id, sessionName, nameSource, title });

  it("prefers an operator-chosen or source-less name", () => {
    expect(of("mine", null, "tab")).toBe("mine");
    expect(of("mine", "user", "tab")).toBe("mine");
  });
  it("falls a derived auto-name back behind the title", () => {
    expect(of("repo-3f", "derived", "tab")).toBe("tab");
    expect(of("repo-3f", "derived", null)).toBe("repo-3f");
  });
  it("treats blanks as absent and falls back to claude <id8>", () => {
    expect(of("  ", null, "tab")).toBe("tab");
    expect(of(null, null, " ")).toBe("claude 01234567");
  });
});

describe("invoke wrappers", () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it("getSessionLedgerReport invokes session_ledger_report", async () => {
    invokeMock.mockResolvedValue(report);
    await expect(getSessionLedgerReport()).resolves.toBe(report);
    expect(invokeMock).toHaveBeenCalledWith("session_ledger_report");
  });

  it("captureSessionLedger invokes session_ledger_capture", async () => {
    invokeMock.mockResolvedValue(capture);
    await expect(captureSessionLedger()).resolves.toBe(capture);
    expect(invokeMock).toHaveBeenCalledWith("session_ledger_capture");
  });

  it("setSessionFinished invokes terminal_session_set_finished and reports a no-op honestly", async () => {
    invokeMock.mockResolvedValue({ success: true, message: null, data: { changed: "marker" } });
    await expect(setSessionFinished("aaaa-1111", true)).resolves.toEqual({
      changed: true,
      message: null,
    });
    expect(invokeMock).toHaveBeenCalledWith("terminal_session_set_finished", {
      claudeSessionId: "aaaa-1111",
      finished: true,
      reason: null,
    });
    // A known id already in that state is `success: true` with
    // `changed: "none"` — a no-op, never reported as a change.
    invokeMock.mockResolvedValue({ success: true, message: null, data: { changed: "none" } });
    await expect(setSessionFinished("aaaa-1111", true)).resolves.toEqual({
      changed: false,
      message: null,
    });
    invokeMock.mockResolvedValue({
      success: false,
      message: "no such session in the lifecycle registry",
      data: null,
    });
    await expect(setSessionFinished("aaaa-1111", false, "oops")).resolves.toEqual({
      changed: false,
      message: "no such session in the lifecycle registry",
    });
  });

  it("propagates a refusal rather than resolving an empty report", async () => {
    invokeMock.mockRejectedValue("lifecycle store not available");
    await expect(captureSessionLedger()).rejects.toBe("lifecycle store not available");
  });
});
