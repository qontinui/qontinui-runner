/**
 * Tests for the registry-driven restore's `fetchOpenRecords` helper — the
 * source-of-truth fetch that replaces the localStorage creation-order binding.
 *
 * vitest runs `environment: "node"` with no React Testing Library, so we mock
 * the IPC surface and exercise the pure-ish helper directly (same precedent as
 * `useTabSessionIdCapture.test.ts`).
 */

import { describe, it, expect, vi, beforeEach } from "vitest";

const mockInvoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
}));

import {
  fetchOpenRecords,
  claimInitForPage,
  buildResumeCmd,
  runVerifiedResume,
  classifyRestoreAction,
} from "./useTerminalInitialization";
import type { TerminalSessionRecord } from "./types";
import type { SessionOpenArgs } from "./sessionRecordArgs";

const rec = (overrides: Partial<TerminalSessionRecord>): TerminalSessionRecord => ({
  claudeSessionId: "sid",
  pageId: "default",
  zoneIndex: 0,
  terminalId: "tab-1",
  openedAt: 1,
  lastSeenAt: 1,
  state: "open",
  ...overrides,
});

describe("fetchOpenRecords", () => {
  beforeEach(() => {
    mockInvoke.mockReset();
  });

  it("returns the open records for the page", async () => {
    mockInvoke.mockResolvedValueOnce({
      success: true,
      message: null,
      data: {
        sessions: [
          rec({ claudeSessionId: "A", zoneIndex: 0, terminalId: "t-A" }),
          rec({ claudeSessionId: "B", zoneIndex: 3, terminalId: "t-B" }),
        ],
      },
    });
    const out = await fetchOpenRecords("default");
    expect(mockInvoke).toHaveBeenCalledWith("terminal_session_list_open");
    expect(out.map((r) => r.claudeSessionId).sort()).toEqual(["A", "B"]);
  });

  it("dedupes a duplicate claudeSessionId down to exactly one record", async () => {
    mockInvoke.mockResolvedValueOnce({
      success: true,
      message: null,
      data: {
        sessions: [
          rec({ claudeSessionId: "DUP", zoneIndex: 2, terminalId: "t-1" }),
          rec({ claudeSessionId: "DUP", zoneIndex: 5, terminalId: "t-2" }),
        ],
      },
    });
    const out = await fetchOpenRecords("default");
    expect(out).toHaveLength(1);
    expect(out[0].claudeSessionId).toBe("DUP");
    // First-wins: keeps the earliest record's zone.
    expect(out[0].zoneIndex).toBe(2);
  });

  it("filters out records belonging to a different page", async () => {
    mockInvoke.mockResolvedValueOnce({
      success: true,
      message: null,
      data: {
        sessions: [
          rec({ claudeSessionId: "A", pageId: "default" }),
          rec({ claudeSessionId: "B", pageId: "page-2" }),
        ],
      },
    });
    const out = await fetchOpenRecords("default");
    expect(out.map((r) => r.claudeSessionId)).toEqual(["A"]);
  });

  it("treats a missing pageId as 'default'", async () => {
    mockInvoke.mockResolvedValueOnce({
      success: true,
      message: null,
      data: {
        sessions: [{ ...rec({ claudeSessionId: "A" }), pageId: undefined as unknown as string }],
      },
    });
    const out = await fetchOpenRecords("default");
    expect(out.map((r) => r.claudeSessionId)).toEqual(["A"]);
  });

  it("passes through whatever the backend returns without re-filtering on state", async () => {
    // The backend (terminal_session_list_open -> restorable_records) owns the
    // state/reason/grace gating: it returns `open` records PLUS in-grace
    // `closed`/`pty-exit` records (graceful-restart case). The client must NOT
    // re-drop the non-open ones — doing so was the restore-on-graceful-restart
    // bug. So a "closed" record the backend chose to return is kept.
    mockInvoke.mockResolvedValueOnce({
      success: true,
      message: null,
      data: {
        sessions: [
          rec({ claudeSessionId: "A", state: "open" }),
          rec({ claudeSessionId: "B", state: "closed", closeReason: "pty-exit" }),
        ],
      },
    });
    const out = await fetchOpenRecords("default");
    expect(out.map((r) => r.claudeSessionId).sort()).toEqual(["A", "B"]);
  });

  it("returns [] when the command throws", async () => {
    mockInvoke.mockRejectedValueOnce(new Error("ipc down"));
    expect(await fetchOpenRecords("default")).toEqual([]);
  });

  it("returns [] when the payload has no sessions array", async () => {
    mockInvoke.mockResolvedValueOnce({ success: true, message: null, data: {} });
    expect(await fetchOpenRecords("default")).toEqual([]);
  });
});

// Phase 3 (mount-hydration lift): the convergence backstop must run once PER
// PAGEID, not once per mount. The single TerminalPage instance now persists
// across terminal-page switches (the session provider was lifted above it), so
// the guard is keyed by pageId.
describe("claimInitForPage (once-per-pageId guard)", () => {
  it("runs init the first time a page is seen", () => {
    const seen = new Set<string>();
    expect(claimInitForPage(seen, "default")).toBe(true);
  });

  it("does NOT re-run init for a page already initialized", () => {
    const seen = new Set<string>();
    expect(claimInitForPage(seen, "default")).toBe(true);
    expect(claimInitForPage(seen, "default")).toBe(false);
    expect(claimInitForPage(seen, "default")).toBe(false);
  });

  it("runs init independently for each pageId (page switch ⇒ new page inits once)", () => {
    const seen = new Set<string>();
    // First visit to page A initializes it.
    expect(claimInitForPage(seen, "page-a")).toBe(true);
    // Switch to page B (no remount) — B initializes once.
    expect(claimInitForPage(seen, "page-b")).toBe(true);
    // Switching back to A does NOT re-init (state survived the switch).
    expect(claimInitForPage(seen, "page-a")).toBe(false);
    // ...and B is likewise already converged.
    expect(claimInitForPage(seen, "page-b")).toBe(false);
  });
});

// Phase 3 (#548) — verified-resume state machine: the restore drain (and the
// operator retry) must only clear the durable restore-pending marker on a
// VERIFIED Claude UI handshake; a failed resume parks the tab in an explicit
// `resumeFailed` retry state with the marker left intact.
describe("runVerifiedResume", () => {
  const CLAUDE_UI = "╭──────────────╮\n│ > │\n╰──────────────╯\n  ? for shortcuts";
  const PLAIN_SHELL = "PS C:\\repo> claude --resume sess-1\r\nPS C:\\repo>";

  /** A refs map whose handle accepts writes (the default writeWhenReady path). */
  const refsWithHandle = (writes: string[]) =>
    new Map([
      [
        "tab-1",
        { current: { writeToTerminal: (text: string) => void writes.push(text) } },
      ],
    ]) as never;

  beforeEach(() => {
    mockInvoke.mockReset();
    mockInvoke.mockResolvedValue({ success: true, message: null, data: null });
  });

  it("verified handshake → clears isReconnecting AND the backend restore-pending marker", async () => {
    const writes: string[] = [];
    const updateTab = vi.fn();
    const out = await runVerifiedResume({
      terminalRefs: refsWithHandle(writes),
      tabId: "tab-1",
      claudeSessionId: "sess-1",
      updateTab,
      verifyOptions: {
        settleMs: 1,
        timeoutMs: 50,
        intervalMs: 1,
        readTail: async () => CLAUDE_UI,
      },
    });
    expect(out).toBe("verified");
    expect(writes.some((w) => w.includes("--resume sess-1\r"))).toBe(true);
    expect(updateTab).toHaveBeenCalledWith("tab-1", {
      isReconnecting: false,
      resumeFailed: false,
    });
    expect(mockInvoke).toHaveBeenCalledWith("terminal_session_clear_restore_pending", {
      claudeSessionId: "sess-1",
    });
  });

  it("persistent failure → parks the tab resumeFailed and does NOT clear the marker", async () => {
    const writes: string[] = [];
    const updateTab = vi.fn();
    const out = await runVerifiedResume({
      terminalRefs: refsWithHandle(writes),
      tabId: "tab-1",
      claudeSessionId: "sess-1",
      updateTab,
      verifyOptions: {
        settleMs: 1,
        timeoutMs: 5,
        intervalMs: 1,
        readTail: async () => PLAIN_SHELL,
      },
    });
    expect(out).toBe("failed");
    // Retried once: the same command typed exactly twice.
    expect(writes.filter((w) => w.includes("--resume sess-1\r"))).toHaveLength(2);
    expect(updateTab).toHaveBeenCalledWith("tab-1", {
      isReconnecting: false,
      resumeFailed: true,
    });
    // The durable open record must stay poll-protected for the retry.
    expect(mockInvoke).not.toHaveBeenCalledWith(
      "terminal_session_clear_restore_pending",
      expect.anything(),
    );
  });

  // Item 3 (boot-restore remediation): the registry re-assert moved from
  // tab-creation time into the VERIFIED branch. A failed resume must leave
  // the original row untouched (original lastSeenAt ⇒ ages out normally).
  const RECORD_OPEN: SessionOpenArgs = {
    claudeSessionId: "sess-1",
    configDir: "C:/claude/.claude-hotmail",
    workingDir: "D:/repo",
    pageId: "default",
    zoneIndex: 3,
    title: "Claude 1",
    terminalId: "tab-1",
  };

  it("verified handshake → re-asserts the OPEN record under the new terminal id (recordOpen)", async () => {
    const writes: string[] = [];
    const updateTab = vi.fn();
    const out = await runVerifiedResume({
      terminalRefs: refsWithHandle(writes),
      tabId: "tab-1",
      claudeSessionId: "sess-1",
      updateTab,
      recordOpen: RECORD_OPEN,
      verifyOptions: {
        settleMs: 1,
        timeoutMs: 50,
        intervalMs: 1,
        readTail: async () => CLAUDE_UI,
      },
    });
    expect(out).toBe("verified");
    expect(mockInvoke).toHaveBeenCalledWith("terminal_session_record_open", RECORD_OPEN);
  });

  it("failed verification → NEVER re-asserts (ghost row keeps its original lastSeenAt)", async () => {
    const writes: string[] = [];
    const updateTab = vi.fn();
    const out = await runVerifiedResume({
      terminalRefs: refsWithHandle(writes),
      tabId: "tab-1",
      claudeSessionId: "sess-1",
      updateTab,
      recordOpen: RECORD_OPEN,
      verifyOptions: {
        settleMs: 1,
        timeoutMs: 5,
        intervalMs: 1,
        readTail: async () => PLAIN_SHELL,
      },
    });
    expect(out).toBe("failed");
    // No record_open ⇒ the registry row's lastSeenAt is untouched and the
    // prune clock / recency cap keep running — ghosts are no longer immortal.
    expect(mockInvoke).not.toHaveBeenCalledWith("terminal_session_record_open", expect.anything());
    expect(mockInvoke).not.toHaveBeenCalledWith(
      "terminal_session_clear_restore_pending",
      expect.anything(),
    );
  });

  it("bogus session id (failure frames inside Claude UI) → failed, marker kept, no re-assert (item 4)", async () => {
    const BOGUS =
      "╭──────────────╮\n│ No conversation found with session ID: sess-1 │\n╰──────────────╯\n  ? for shortcuts";
    const writes: string[] = [];
    const updateTab = vi.fn();
    const out = await runVerifiedResume({
      terminalRefs: refsWithHandle(writes),
      tabId: "tab-1",
      claudeSessionId: "sess-1",
      updateTab,
      recordOpen: RECORD_OPEN,
      verifyOptions: {
        settleMs: 1,
        timeoutMs: 50,
        intervalMs: 1,
        readTail: async () => BOGUS,
      },
    });
    expect(out).toBe("failed");
    expect(updateTab).toHaveBeenCalledWith("tab-1", {
      isReconnecting: false,
      resumeFailed: true,
    });
    expect(mockInvoke).not.toHaveBeenCalledWith("terminal_session_record_open", expect.anything());
    expect(mockInvoke).not.toHaveBeenCalledWith(
      "terminal_session_clear_restore_pending",
      expect.anything(),
    );
  });
});

// Phase 4 (autonomous restore): the auto-resume GATE is
// `origin === "authoritative" && confirmed`. A CONFIRMED authoritative record
// auto-resumes with NO operator click; an authoritative-but-PROVISIONAL record
// is a phantom shell → terminal-only restore (no resume, no banner);
// reconciled/absent origins stay quarantined behind the one-click confirm.
describe("classifyRestoreAction (Phase 4 confirmed-authoritative auto-resume gate)", () => {
  it("CONFIRMED authoritative binding auto-resumes with no operator click", () => {
    expect(
      classifyRestoreAction({
        claudeSessionId: "sess-1",
        origin: "authoritative",
        confirmedAt: 1_700_000_000_000,
      }),
    ).toBe("auto-resume");
  });

  it("authoritative-but-PROVISIONAL (no confirmedAt) is a phantom shell ⇒ terminal-only (no resume, no banner)", () => {
    expect(classifyRestoreAction({ claudeSessionId: "sess-1", origin: "authoritative" })).toBe(
      "terminal-only",
    );
  });

  it("reconciled binding is quarantined (backstop guess can name a foreign session)", () => {
    expect(classifyRestoreAction({ claudeSessionId: "sess-1", origin: "reconciled" })).toBe(
      "quarantine",
    );
  });

  it("a confirmed RECONCILED row is still quarantined (confirmation upgrades only authoritative rows)", () => {
    expect(
      classifyRestoreAction({
        claudeSessionId: "sess-1",
        origin: "reconciled",
        confirmedAt: 1_700_000_000_000,
      }),
    ).toBe("quarantine");
  });

  it("absent origin (pre-field record) reads as reconciled ⇒ quarantined", () => {
    expect(classifyRestoreAction({ claudeSessionId: "sess-1" })).toBe("quarantine");
  });

  it("shell-unsafe session ids are skipped outright, never typed or quarantined", () => {
    expect(
      classifyRestoreAction({
        claudeSessionId: "bad; rm -rf /",
        origin: "authoritative",
        confirmedAt: 1,
      }),
    ).toBe("skip-invalid");
  });
});

// #548 item 3 — the typed resume must suppress the CLI's "Resume from
// summary?" picker under the default full-resume policy (env thresholds; no
// CLI flag / settings key short of the global "Don't ask again" exists), and
// must leave it answerable under the opt-in summary policy.
describe("buildResumeCmd (resume-size picker policy)", () => {
  it("default 'full' policy raises both resume thresholds so the picker never shows", () => {
    const cmd = buildResumeCmd("sess-1", undefined, "full");
    expect(cmd).toContain('CLAUDE_CODE_RESUME_TOKEN_THRESHOLD="999999999"');
    expect(cmd).toContain('CLAUDE_CODE_RESUME_THRESHOLD_MINUTES="999999999"');
    expect(cmd).toContain("--resume sess-1\r");
  });

  it("'summary' policy leaves the thresholds alone (picker shows; the loop answers it)", () => {
    const cmd = buildResumeCmd("sess-1", undefined, "summary");
    expect(cmd).not.toContain("CLAUDE_CODE_RESUME_TOKEN_THRESHOLD");
    expect(cmd).toBe("claude --permission-mode bypassPermissions --resume sess-1\r");
  });

  it("keeps the CLAUDE_CONFIG_DIR prefix alongside the threshold vars", () => {
    const cmd = buildResumeCmd("sess-1", "C:/claude/.claude-hotmail", "full");
    expect(cmd).toContain('CLAUDE_CONFIG_DIR="C:/claude/.claude-hotmail"');
    expect(cmd).toContain("CLAUDE_CODE_RESUME_TOKEN_THRESHOLD");
    expect(cmd).toContain("--resume sess-1\r");
  });

  it("resumes autonomously — bypassPermissions so an unattended restore never stalls on a permission prompt (#547 union)", () => {
    for (const policy of ["full", "summary"] as const) {
      expect(buildResumeCmd("abc-123", undefined, policy)).toContain(
        "claude --permission-mode bypassPermissions --resume abc-123",
      );
    }
  });

  // G2 (Phase 1 configDir capture): now that every capture path persists a
  // non-null configDir, the resume must emit the CLAUDE_CONFIG_DIR prefix
  // whenever an account is bound, and NONE when it isn't — isolated here from
  // the threshold vars via the 'summary' policy so the config-dir branch alone
  // is asserted.
  it("emits the CLAUDE_CONFIG_DIR prefix iff an account dir is bound (config-dir branch, no thresholds)", () => {
    const bound = buildResumeCmd("sess-1", "C:/claude/.claude-qontinui", "summary");
    expect(bound).toContain('CLAUDE_CONFIG_DIR="C:/claude/.claude-qontinui"');
    expect(bound).not.toContain("CLAUDE_CODE_RESUME_TOKEN_THRESHOLD");
    expect(bound).toContain("--resume sess-1\r");

    // No account bound ⇒ no prefix at all (bare command).
    const unbound = buildResumeCmd("sess-1", undefined, "summary");
    expect(unbound).not.toContain("CLAUDE_CONFIG_DIR");
    expect(unbound).toBe("claude --permission-mode bypassPermissions --resume sess-1\r");
  });
});
