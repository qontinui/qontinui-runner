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

import { fetchOpenRecords, claimInitForPage, buildResumeCmd } from "./useTerminalInitialization";
import type { TerminalSessionRecord } from "./types";

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

describe("buildResumeCmd (boot-restore resume command)", () => {
  it("resumes autonomously — bypassPermissions so an unattended restore never stalls on a permission prompt", () => {
    const cmd = buildResumeCmd("abc-123", undefined);
    expect(cmd).toBe("claude --permission-mode bypassPermissions --resume abc-123\r");
  });

  it("prefixes CLAUDE_CONFIG_DIR while keeping the bypass form", () => {
    vi.stubGlobal("navigator", { platform: "Win32" });
    try {
      const cmd = buildResumeCmd("abc-123", "C:\\claude\\.claude-hotmail");
      expect(cmd).toBe(
        '$env:CLAUDE_CONFIG_DIR="C:\\claude\\.claude-hotmail"; ' +
          "claude --permission-mode bypassPermissions --resume abc-123\r",
      );
    } finally {
      vi.unstubAllGlobals();
    }
  });
});
