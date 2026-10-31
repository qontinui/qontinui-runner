/**
 * Tests for the registry-driven restore's `fetchOpenRecords` helper — the
 * source-of-truth fetch that replaces the localStorage creation-order binding.
 *
 * vitest runs `environment: "node"` with no React Testing Library, so we mock
 * the IPC surface and exercise the pure-ish helper directly (same precedent as
 * `useTabSessionIdCapture.test.ts`).
 */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

const mockInvoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
}));

import {
  fetchOpenRecords,
  fetchRestoreSet,
  waitForHolderSweep,
  recheckRefusedAfterSweep,
  HOLDER_SWEEP_RECHECK_ATTEMPTS,
  fetchHolderSweepStatus,
  splitRestoreByHold,
  onHolderSessionReleased,
  ownsReleasedPage,
  RestoreRerunQueue,
  ABORT_RETRY_BASE_MS,
  ABORT_RETRY_LIMIT,
  RerunTimers,
  beginRestoreRun,
  scheduleRestoreRerun,
  settleRestoreRun,
  type RestoreRerunWiring,
  ColdResumeGuard,
  mayClaimRecordedZone,
  claimInitForPage,
  buildResumeCmd,
  runVerifiedResume,
  classifyRestoreAction,
  resolveRestoreAccount,
  restoreTabName,
  recordBelongsToRestore,
  reportTreeReset,
  decideReconnectOutcome,
  decideColdResume,
  reconnectedTabFor,
  decideDrainSkipDisposition,
  applyDrainSkip,
  holdNeedsAccountIfNotLive,
  applyNeedsAccountNote,
  type NeedsAccountRecord,
} from "./useTerminalInitialization";
import type { TerminalSessionRecord } from "./types";
import type { SessionOpenArgs } from "./sessionRecordArgs";

/** The default Claude home the account rule is judged against. */
const HOME = "/home/u/.claude";

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
    expect(mockInvoke).toHaveBeenCalledWith("terminal_session_list_open", { purpose: "restore" });
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

// P0 tree-reset observability: the mount effect fires a durable
// terminal_report_tree_reset on EVERY mount, fire-and-forget — the report must
// never be able to affect the initialization flow.
describe("reportTreeReset (tree-reset observability)", () => {
  beforeEach(() => {
    mockInvoke.mockReset();
  });

  it("reports the mount with the collected payload (open-record count included)", async () => {
    mockInvoke.mockImplementation((cmd: unknown) => {
      if (cmd === "terminal_session_list_open") {
        return Promise.resolve({
          success: true,
          message: null,
          data: { sessions: [rec({ claudeSessionId: "A" }), rec({ claudeSessionId: "B" })] },
        });
      }
      return Promise.resolve({ success: true, message: null, data: null });
    });
    await reportTreeReset({
      mountNumber: 2,
      authenticated: true,
      pageIds: ["default", "p2"],
      clientTs: 1_700_000_000_000,
    });
    expect(mockInvoke).toHaveBeenCalledWith(
      "terminal_report_tree_reset",
      expect.objectContaining({
        mountNumber: 2,
        authenticated: true,
        pageIds: ["default", "p2"],
        openRecordCount: 2,
        // Same-document proof: rows sharing a timeOrigin belong to one
        // webview document — the remount-vs-fresh-load discriminator.
        timeOrigin: expect.any(Number),
        clientTs: 1_700_000_000_000,
      }),
    );
  });

  it("mount #1 is reported too (consumers filter on mountNumber)", async () => {
    mockInvoke.mockResolvedValue({ success: true, message: null, data: null });
    await reportTreeReset({ mountNumber: 1, pageIds: [] });
    expect(mockInvoke).toHaveBeenCalledWith(
      "terminal_report_tree_reset",
      expect.objectContaining({ mountNumber: 1, authenticated: undefined }),
    );
  });

  it("still reports when the open-record count fetch fails (partial payload)", async () => {
    mockInvoke.mockImplementation((cmd: unknown) =>
      cmd === "terminal_session_list_open"
        ? Promise.reject(new Error("ipc down"))
        : Promise.resolve({ success: true, message: null, data: null }),
    );
    await reportTreeReset({ mountNumber: 3, pageIds: ["default"] });
    expect(mockInvoke).toHaveBeenCalledWith(
      "terminal_report_tree_reset",
      expect.objectContaining({ mountNumber: 3, openRecordCount: undefined }),
    );
  });

  it("swallows a rejected report — never throws into the mount effect", async () => {
    mockInvoke.mockRejectedValue(new Error("ipc down"));
    await expect(
      reportTreeReset({ mountNumber: 2, pageIds: ["default"] }),
    ).resolves.toBeUndefined();
  });
});

// Orphan-page adoption (2026-07-13 session-loss incident): a record whose
// pageId names no live page must be adopted by the first restoring page —
// but ONLY when it classifies auto-resume. Everything else keeps today's
// strict page filter.
describe("recordBelongsToRestore (orphan-page adoption)", () => {
  const confirmed = (page: string): TerminalSessionRecord =>
    rec({
      claudeSessionId: "579f119e-4dad-443c-9c11-e036f391d4dc",
      pageId: page,
      origin: "authoritative",
      confirmedAt: 123,
      configDir: "/home/u/.claude-x",
    });

  it("matching page always restores (no adoption involved)", () => {
    expect(recordBelongsToRestore(confirmed("page-a"), "page-a", ["page-a"], false)).toBe(true);
  });

  it("record on ANOTHER live page is never taken (that page owns it)", () => {
    expect(recordBelongsToRestore(confirmed("page-b"), "page-a", ["page-a", "page-b"], true)).toBe(
      false,
    );
  });

  it("orphan (page not in layout) + auto-resume class ⇒ adopted when this page is the adopter", () => {
    expect(recordBelongsToRestore(confirmed("default"), "page-a", ["page-a"], true)).toBe(true);
    // A deleted page's id is just as orphaned as "default".
    expect(recordBelongsToRestore(confirmed("gone-page"), "page-a", ["page-a"], true)).toBe(true);
  });

  it("an orphan whose ACCOUNT is unknown (needs-account) is adopted too, so it gets asked about", () => {
    const needsAccount = { ...confirmed("gone-page"), configDir: undefined };
    expect(classifyRestoreAction(needsAccount, HOME)).toBe("needs-account");
    expect(recordBelongsToRestore(needsAccount, "page-a", ["page-a"], true)).toBe(true);
  });

  it("orphan is NOT adopted by a non-adopter page (one adopter per boot)", () => {
    expect(recordBelongsToRestore(confirmed("default"), "page-a", ["page-a"], false)).toBe(false);
  });

  it("orphan on the FRESH-INSTALL default page is not an orphan (default is a live page)", () => {
    // knownPageIds includes "default" ⇒ default-page records belong to the
    // default page's own restore run, never adopted elsewhere.
    expect(
      recordBelongsToRestore(confirmed("default"), "page-a", ["default", "page-a"], true),
    ).toBe(false);
    expect(recordBelongsToRestore(confirmed("default"), "default", ["default"], true)).toBe(true);
  });

  it("unconfirmed / quarantine-tier orphans are never adopted (agent shells stay dead)", () => {
    const unconfirmed = rec({
      claudeSessionId: "1fce6334-0000-4000-8000-000000000000",
      pageId: "default",
      origin: "authoritative",
      confirmedAt: undefined,
    });
    expect(recordBelongsToRestore(unconfirmed, "page-a", ["page-a"], true)).toBe(false);
    const reconciled = rec({
      claudeSessionId: "2fce6334-0000-4000-8000-000000000000",
      pageId: "default",
      origin: "reconciled",
      confirmedAt: 123,
    });
    expect(recordBelongsToRestore(reconciled, "page-a", ["page-a"], true)).toBe(false);
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

// P1 restore idempotence, defect (a): `reconnectToExistingSessions` returns
// `null` ONLY for an INDETERMINATE read (invoke failed / unsuccessful /
// malformed response) — never for "definitely nothing" (that's `[]`).
// Coercing null to [] sent every open record down the cold `claude --resume`
// path while its previous terminal generation was still alive, orphaning a
// PTY generation per restore pass. null must ABORT the restore.
describe("decideReconnectOutcome (P1 null → abort-restore)", () => {
  it("aborts restore when the reconnect result is indeterminate (null)", () => {
    expect(decideReconnectOutcome(null)).toEqual({ kind: "abort-restore" });
  });

  it("proceeds on an EMPTY list — a definitive cold start is not an abort", () => {
    expect(decideReconnectOutcome([])).toEqual({
      kind: "proceed",
      reconnectedTabIds: [],
    });
  });

  it("proceeds with the reconnected ids when some PTYs survived", () => {
    expect(decideReconnectOutcome(["t-1", "t-2"])).toEqual({
      kind: "proceed",
      reconnectedTabIds: ["t-1", "t-2"],
    });
  });
});

// P1 restore idempotence, defect (b): even after a successful reconnect, a
// record's session can be alive under a process the reconnect did not return.
// The cold auto-resume path must consult the live-session registry and NEVER
// respawn an id that is already hosted — and must fail CLOSED (skip) when the
// registry could not be read.
describe("decideColdResume (P1 liveness gate before cold respawn)", () => {
  it("respawns only when the id is definitively not live", () => {
    expect(decideColdResume(new Set(["other"]), "sess-1")).toBe("respawn");
    expect(decideColdResume(new Set(), "sess-1")).toBe("respawn");
  });

  it("skips (rebind-not-respawn) when a live process already hosts the id", () => {
    expect(decideColdResume(new Set(["sess-1"]), "sess-1")).toBe("skip-alive");
  });

  it("fails CLOSED when the registry read failed — unknown liveness never respawns", () => {
    expect(decideColdResume(null, "sess-1")).toBe("skip-unknown");
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
      ["tab-1", { current: { writeToTerminal: (text: string) => void writes.push(text) } }],
    ]) as never;

  beforeEach(() => {
    mockInvoke.mockReset();
    // The pane probe reads a plain shell (no claude), so the typing paths run.
    mockInvoke.mockImplementation(async (cmd: string, args?: { sessionId?: string }) => {
      if (cmd === "terminal_probe_claude")
        return { success: true, message: null, data: { state: "absent", sessionIds: [] } };
      if (cmd === "build_ai_resume_command")
        return {
          success: true,
          message: null,
          data: {
            command: `claude --permission-mode bypassPermissions --resume ${args?.sessionId}`,
          },
        };
      return { success: true, message: null, data: null };
    });
  });

  it("a claude that is NOT provably this session → types nothing, parks resumeFailed, no re-assert, marker kept", async () => {
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
        readTail: async () => "",
        probeClaude: async () => ({ state: "live", sessionIds: ["sess-other"] }),
      },
    });
    expect(out).toBe("failed");
    expect(writes).toEqual([]);
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

  it("never asks for the DEFAULT home as CLAUDE_CONFIG_DIR — any other account is asked for", async () => {
    // The backend renders the typed command (`build_ai_resume_command`), so the
    // frontend's half of the rule is the dir it ASKS for: the default home goes
    // as `null` (no CLAUDE_CONFIG_DIR), any other account verbatim.
    const askedDir = async (configDir: string) => {
      mockInvoke.mockClear();
      const writes: string[] = [];
      await runVerifiedResume({
        terminalRefs: refsWithHandle(writes),
        tabId: "tab-1",
        claudeSessionId: "sess-1",
        configDir,
        updateTab: vi.fn(),
        defaultConfigHome: async () => "/home/u/.claude",
        verifyOptions: {
          settleMs: 1,
          timeoutMs: 50,
          intervalMs: 1,
          readTail: async () => CLAUDE_UI,
        },
      });
      expect(writes.some((w) => w.includes("--resume sess-1"))).toBe(true);
      const call = mockInvoke.mock.calls.find(([cmd]) => cmd === "build_ai_resume_command");
      return (call?.[1] as { configDir?: string | null } | undefined)?.configDir;
    };
    expect(await askedDir("/home/u/.claude/")).toBeNull();
    expect(await askedDir("/home/u/.claude-x")).toBe("/home/u/.claude-x");
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
// reconciled/absent origins are too weak to act on and restore terminal-only
// as well — the runner does nothing further with a guessed id.
describe("classifyRestoreAction (Phase 4 confirmed-authoritative auto-resume gate)", () => {
  it("CONFIRMED authoritative binding auto-resumes with no operator click", () => {
    expect(
      classifyRestoreAction(
        {
          claudeSessionId: "sess-1",
          origin: "authoritative",
          confirmedAt: 1_700_000_000_000,
          configDir: "/home/u/.claude-x",
        },
        HOME,
      ),
    ).toBe("auto-resume");
  });

  it("authoritative-but-PROVISIONAL (no confirmedAt) is a phantom shell ⇒ terminal-only (no resume, no banner)", () => {
    expect(
      classifyRestoreAction({ claudeSessionId: "sess-1", origin: "authoritative" }, HOME),
    ).toBe("terminal-only");
  });

  it("reconciled binding restores terminal-only (backstop guess can name a foreign session, too weak to act on)", () => {
    expect(classifyRestoreAction({ claudeSessionId: "sess-1", origin: "reconciled" }, HOME)).toBe(
      "terminal-only",
    );
  });

  it("a confirmed RECONCILED row is still terminal-only (confirmation upgrades only authoritative/observed rows)", () => {
    expect(
      classifyRestoreAction(
        {
          claudeSessionId: "sess-1",
          origin: "reconciled",
          confirmedAt: 1_700_000_000_000,
        },
        HOME,
      ),
    ).toBe("terminal-only");
  });

  it("absent origin (pre-field record) reads as reconciled ⇒ terminal-only", () => {
    expect(classifyRestoreAction({ claudeSessionId: "sess-1" }, HOME)).toBe("terminal-only");
  });

  it("shell-unsafe session ids are skipped outright, never typed", () => {
    expect(
      classifyRestoreAction(
        {
          claudeSessionId: "bad; rm -rf /",
          origin: "authoritative",
          confirmedAt: 1,
        },
        HOME,
      ),
    ).toBe("skip-invalid");
  });
});

// Plan 2026-10-04-runner-session-roster-restore-picker, Phase 3: a restore
// never resumes under a GUESSED account. The recorded configDir wins; with none,
// a UNIQUE transcript holder is evidence; zero / several holders, or a rejected
// dir, make the record `needs-account` — Claude only.
describe("classifyRestoreAction — needs-account (no silent default account)", () => {
  const CONFIRMED_CLAUDE = {
    claudeSessionId: "sess-1",
    origin: "authoritative",
    confirmedAt: 1,
    provider: "claude",
    transcriptExists: true,
  } as const;

  it("a recorded configDir auto-resumes whatever the transcript evidence says", () => {
    expect(
      classifyRestoreAction(
        {
          ...CONFIRMED_CLAUDE,
          configDir: "/home/u/.claude-x",
          transcriptConfigDirCount: 2,
        },
        HOME,
      ),
    ).toBe("auto-resume");
  });

  it("a recorded configDir rejected by sanitizeConfigDir is needs-account, never the default", () => {
    expect(classifyRestoreAction({ ...CONFIRMED_CLAUDE, configDir: "/x/$(evil)" }, HOME)).toBe(
      "needs-account",
    );
  });

  it("no recorded dir + a UNIQUE transcript holder auto-resumes (evidence, not a guess)", () => {
    expect(
      classifyRestoreAction(
        {
          ...CONFIRMED_CLAUDE,
          transcriptConfigDirCount: 1,
          transcriptConfigDir: "/home/u/.claude-niklas",
          transcriptConfigDirIsDefault: false,
        },
        HOME,
      ),
    ).toBe("auto-resume");
  });

  it("no recorded dir + SEVERAL transcript holders is needs-account", () => {
    expect(classifyRestoreAction({ ...CONFIRMED_CLAUDE, transcriptConfigDirCount: 2 }, HOME)).toBe(
      "needs-account",
    );
  });

  it("no recorded dir + ZERO transcript holders is needs-account", () => {
    expect(
      classifyRestoreAction(
        {
          ...CONFIRMED_CLAUDE,
          transcriptExists: undefined,
          transcriptConfigDirCount: 0,
        },
        HOME,
      ),
    ).toBe("needs-account");
  });

  it("no recorded dir and an UNPROBED location is needs-account, never a default-account resume", () => {
    expect(classifyRestoreAction(CONFIRMED_CLAUDE, HOME)).toBe("needs-account");
  });

  it("applies only to a would-be auto-resume: a provisional row stays terminal-only", () => {
    expect(
      classifyRestoreAction(
        {
          ...CONFIRMED_CLAUDE,
          confirmedAt: undefined,
          transcriptConfigDirCount: 2,
        },
        HOME,
      ),
    ).toBe("terminal-only");
  });
});

describe("resolveRestoreAccount", () => {
  it("takes the recorded dir first", () => {
    expect(
      resolveRestoreAccount({ configDir: "/a/.claude-x", transcriptConfigDirCount: 2 }, HOME),
    ).toEqual({ kind: "resolved", recordDir: "/a/.claude-x", typedDir: "/a/.claude-x" });
  });

  it("a blank recorded dir counts as absent", () => {
    expect(
      resolveRestoreAccount(
        {
          configDir: "  ",
          transcriptConfigDirCount: 1,
          transcriptConfigDir: "/a/.claude-y",
          transcriptConfigDirIsDefault: false,
        },
        HOME,
      ),
    ).toEqual({ kind: "resolved", recordDir: "/a/.claude-y", typedDir: "/a/.claude-y" });
  });

  it("the DEFAULT home types NO CLAUDE_CONFIG_DIR, and only the record keeps its path", () => {
    expect(
      resolveRestoreAccount(
        {
          transcriptConfigDirCount: 1,
          transcriptConfigDir: "/home/u/.claude",
          transcriptConfigDirIsDefault: true,
        },
        HOME,
      ),
    ).toEqual({ kind: "resolved", recordDir: "/home/u/.claude", typedDir: undefined });
    expect(resolveRestoreAccount({ configDir: "/home/u/.claude/" }, HOME)).toEqual({
      kind: "resolved",
      recordDir: "/home/u/.claude/",
      typedDir: undefined,
    });
  });

  it("a default home that is not shell-safe still resumes — it is never typed", () => {
    const home = "/home/Zoë O'Brien/.claude";
    expect(resolveRestoreAccount({ configDir: home }, home)).toEqual({
      kind: "resolved",
      recordDir: home,
      typedDir: undefined,
    });
    expect(
      classifyRestoreAction(
        {
          claudeSessionId: "sess-1",
          origin: "authoritative",
          confirmedAt: 1,
          provider: "claude",
          transcriptExists: true,
          configDir: home,
        },
        home,
      ),
    ).toBe("auto-resume");
  });

  it("with the default home unreadable, a dir that may be it is needs-account, never typed", () => {
    expect(resolveRestoreAccount({ configDir: "/home/u/.claude" }, null)).toEqual({
      kind: "unknown",
      reason: "default-home-unknown",
    });
    // Any other account is still typed as itself.
    expect(resolveRestoreAccount({ configDir: "/home/u/.claude-x" }, null)).toEqual({
      kind: "resolved",
      recordDir: "/home/u/.claude-x",
      typedDir: "/home/u/.claude-x",
    });
  });

  it("names the reason when the account is unknown", () => {
    expect(resolveRestoreAccount({ configDir: "C:/a`b`" }, HOME)).toEqual({
      kind: "unknown",
      reason: "config-dir-rejected",
    });
    expect(resolveRestoreAccount({ transcriptConfigDirCount: 0 }, HOME)).toEqual({
      kind: "unknown",
      reason: "no-transcript-holder",
    });
    expect(resolveRestoreAccount({ transcriptConfigDirCount: 3 }, HOME)).toEqual({
      kind: "unknown",
      reason: "ambiguous-transcript",
    });
    expect(
      resolveRestoreAccount(
        {
          transcriptConfigDirCount: 1,
          transcriptConfigDir: "/x/$HOME",
          transcriptConfigDirIsDefault: false,
        },
        HOME,
      ),
    ).toEqual({ kind: "unknown", reason: "config-dir-rejected" });
  });

  it("no recorded dir and no probe is unknown — nothing names the account", () => {
    expect(resolveRestoreAccount({}, HOME)).toEqual({
      kind: "unknown",
      reason: "transcript-unprobed",
    });
  });
});

// The restored tab is named by THE display-name rule (`displayNameOf`), not
// the bare tab `title` (just "claude" in about half of all rows).
describe("restoreTabName (display-name rule)", () => {
  const base = { claudeSessionId: "0123456789abcdef" };

  it("an operator-chosen name wins over the title", () => {
    expect(restoreTabName({ ...base, sessionName: "fix the bug", title: "claude" })).toBe(
      "fix the bug",
    );
    expect(
      restoreTabName({
        ...base,
        sessionName: "fix the bug",
        nameSource: "operator",
        title: "claude",
      }),
    ).toBe("fix the bug");
  });

  it("a DERIVED auto-name falls back to the title", () => {
    expect(
      restoreTabName({
        ...base,
        sessionName: "qontinui-coord-c7",
        nameSource: "derived",
        title: "runner work",
      }),
    ).toBe("runner work");
  });

  it("a derived name is still used when there is no title", () => {
    expect(
      restoreTabName({ ...base, sessionName: "qontinui-coord-c7", nameSource: "derived" }),
    ).toBe("qontinui-coord-c7");
  });

  it("with neither, the id prefix", () => {
    expect(restoreTabName(base)).toBe("claude 01234567");
  });
});

// The Claude resume is RENDERED BY THE BACKEND (review H1): the config dir and
// permission posture are its decisions. The CLI-shape assertions (#548 item 3
// thresholds, the bypass/allow-list flag, the gateway dir) live with the Rust
// renderer `commands::config::render_ai_resume_command`; these pin the
// frontend half — what is asked for, and that nothing is typed on a refusal.
describe("buildResumeCmd (backend-rendered resume)", () => {
  // `isWindows` comes from `navigator.platform`, and Node has a real global
  // `navigator` that reports the HOST — so without a pin these expectations
  // passed on Linux CI and failed on every Windows dev box.
  beforeEach(() => {
    mockInvoke.mockReset();
    vi.stubGlobal("navigator", { platform: "Linux x86_64" });
  });
  afterEach(() => vi.unstubAllGlobals());

  it("tells the backend the host is Windows when the platform says so", async () => {
    vi.stubGlobal("navigator", { platform: "Win32" });
    mockInvoke.mockResolvedValueOnce({ success: true, message: null, data: { command: "W" } });
    await buildResumeCmd("sess-1", undefined, "summary");
    expect(mockInvoke).toHaveBeenCalledWith("build_ai_resume_command", {
      sessionId: "sess-1",
      provider: "claude",
      configDir: null,
      isWindows: true,
      fullResume: false,
    });
  });

  it("asks the backend with the recorded dir and the full-resume policy, then appends Enter", async () => {
    mockInvoke.mockResolvedValueOnce({
      success: true,
      message: null,
      data: { command: "RENDERED" },
    });
    const cmd = await buildResumeCmd("sess-1", "C:/claude/.claude-hotmail", "full");
    expect(cmd).toBe("RENDERED\r");
    expect(mockInvoke).toHaveBeenCalledWith("build_ai_resume_command", {
      sessionId: "sess-1",
      provider: "claude",
      configDir: "C:/claude/.claude-hotmail",
      isWindows: false,
      fullResume: true,
    });
  });

  it("'summary' policy asks for no threshold env; an unbound dir is sent as null", async () => {
    mockInvoke.mockResolvedValueOnce({ success: true, message: null, data: { command: "X" } });
    await buildResumeCmd("sess-1", undefined, "summary");
    expect(mockInvoke).toHaveBeenCalledWith("build_ai_resume_command", {
      sessionId: "sess-1",
      provider: "claude",
      configDir: null,
      isWindows: false,
      fullResume: false,
    });
  });

  // Review N7: a non-Claude provider's resume is rendered by the backend too
  // (so a provider refusal under a gateway applies), never assembled here.
  it("routes a non-Claude provider through the backend as well", async () => {
    mockInvoke.mockResolvedValueOnce({ success: true, message: null, data: { command: "G" } });
    expect(await buildResumeCmd("sess-1", undefined, "summary", "gemini")).toBe("G\r");
    expect(mockInvoke).toHaveBeenCalledWith("build_ai_resume_command", {
      sessionId: "sess-1",
      provider: "gemini",
      configDir: null,
      isWindows: false,
      fullResume: false,
    });
  });

  it("a backend refusal rejects — the caller types nothing", async () => {
    mockInvoke.mockResolvedValueOnce({
      success: false,
      message: "model gateway unresolved",
      data: null,
    });
    await expect(buildResumeCmd("sess-1", undefined, "full")).rejects.toThrow(
      "model gateway unresolved",
    );
  });

  it("runVerifiedResume types nothing and parks the tab when the backend refuses to render", async () => {
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === "build_ai_resume_command"
        ? { success: false, message: "refused", data: null }
        : { success: true, message: null, data: null },
    );
    const writes: string[] = [];
    const updateTab = vi.fn();
    const out = await runVerifiedResume({
      terminalRefs: new Map([
        ["tab-1", { current: { writeToTerminal: (t: string) => void writes.push(t) } }],
      ]) as never,
      tabId: "tab-1",
      claudeSessionId: "sess-1",
      updateTab,
    });
    expect(out).toBe("failed");
    expect(writes).toEqual([]);
    expect(updateTab).toHaveBeenCalledWith("tab-1", { isReconnecting: false, resumeFailed: true });
  });
});

// ---------------------------------------------------------------------------
// Manual-test-loop iteration 10, item 1 — the STRANDED RESTORE MARKER.
//
// The drain used to answer a non-`respawn` decision with a bare
// `updateTab(tabId, {isReconnecting:false}); continue;`. The durable
// `restore_pending_at` marker stamped before the drain therefore stayed SET on
// a record still pointing at its DEAD pre-restart terminal id, which the
// backend poll can never match — and the poll clears a stale marker ONLY on the
// `KeepAlive` branch, which an unmatched record cannot reach. Result: the row
// sat at `tier=failed, pendingAt=set` forever, with `resumeFailed` never set so
// `ResumeFailedBanner` never rendered it either. Silent, permanent, invisible.
// ---------------------------------------------------------------------------
describe("decideDrainSkipDisposition (item 1 — never strand the marker)", () => {
  it("skip-unknown surfaces the actionable retry banner", () => {
    const d = decideDrainSkipDisposition("skip-unknown");
    expect(d.tabUpdates).toEqual({ isReconnecting: false, resumeFailed: true });
  });

  it("skip-alive surfaces the informational note — retrying would FORK the live transcript", () => {
    const d = decideDrainSkipDisposition("skip-alive");
    expect(d.tabUpdates).toEqual({ isReconnecting: false, restoreTerminalOnly: true });
    expect(d.tabUpdates.resumeFailed).toBeUndefined();
  });

  it("BOTH skips rebind the record and clear the marker — the invariant that was violated", () => {
    for (const decision of ["skip-alive", "skip-unknown"] as const) {
      const d = decideDrainSkipDisposition(decision);
      expect(d.rebindToRestoredTab).toBe(true);
      expect(d.clearRestorePending).toBe(true);
      // Whatever else it does, it must never leave the pane spinning.
      expect(d.tabUpdates.isReconnecting).toBe(false);
    }
  });
});

describe("applyDrainSkip (item 1 — the wiring, not just the decision)", () => {
  /** The row state a caller can observe after the skip, as the two IPCs describe it. */
  const runSkip = async (decision: "skip-alive" | "skip-unknown") => {
    const calls: Array<[string, Record<string, unknown> | undefined]> = [];
    const updateTab = vi.fn();
    await applyDrainSkip({
      decision,
      tabId: "tab-new",
      claudeSessionId: "sess-1",
      zoneIndex: 3,
      updateTab,
      invokeFn: async (cmd, args) => {
        calls.push([cmd, args]);
        return { success: true };
      },
    });
    return { calls, updateTab };
  };

  it("skip-unknown (fetchLiveClaudeSessionIds returned null) rebinds, clears the marker, and shows the banner", async () => {
    const { calls, updateTab } = await runSkip("skip-unknown");
    expect(calls.map((c) => c[0])).toEqual([
      "terminal_session_rebind_terminal",
      "terminal_session_clear_restore_pending",
    ]);
    expect(calls[0][1]).toEqual({
      claudeSessionId: "sess-1",
      terminalId: "tab-new",
      zoneIndex: 3,
    });
    expect(calls[1][1]).toEqual({ claudeSessionId: "sess-1" });
    expect(updateTab).toHaveBeenCalledWith("tab-new", {
      isReconnecting: false,
      resumeFailed: true,
    });
  });

  it("rebind runs BEFORE the clear — the reverse would relabel a skipped restore 'resumed'", async () => {
    const { calls } = await runSkip("skip-alive");
    expect(calls[0][0]).toBe("terminal_session_rebind_terminal");
    expect(calls[1][0]).toBe("terminal_session_clear_restore_pending");
  });

  it("a failing rebind does NOT abort the marker clear — the stranding must not survive an IPC hiccup", async () => {
    const seen: string[] = [];
    const updateTab = vi.fn();
    await applyDrainSkip({
      decision: "skip-unknown",
      tabId: "tab-new",
      claudeSessionId: "sess-1",
      zoneIndex: 0,
      updateTab,
      invokeFn: async (cmd) => {
        seen.push(cmd);
        if (cmd === "terminal_session_rebind_terminal") throw new Error("ipc down");
        return { success: true };
      },
    });
    expect(seen).toContain("terminal_session_clear_restore_pending");
  });
});

/**
 * Plan `2026-09-13-drained-runner-never-reaches-idle`, Phase 3: while coord's
 * device drain holds, `terminal_session_list_open` withholds the restore set
 * with a `deferredByDrain` marker. That must never read as "no sessions".
 */
describe("fetchRestoreSet — coord device drain", () => {
  beforeEach(() => {
    mockInvoke.mockReset();
  });

  it("surfaces a drain deferral distinctly from an empty restore set", async () => {
    mockInvoke.mockResolvedValueOnce({
      success: true,
      message: "coord has drained this device",
      data: {
        sessions: [],
        deferredByDrain: { state: "drained", reason: "coord has drained this device" },
      },
    });
    const out = await fetchRestoreSet("default");
    expect(out.records).toEqual([]);
    expect(out.deferredByDrain).toBe("coord has drained this device");
  });

  it("falls back to a generic reason when the marker carries none", async () => {
    mockInvoke.mockResolvedValueOnce({
      success: true,
      data: { sessions: [], deferredByDrain: { state: "unknown" } },
    });
    expect((await fetchRestoreSet("default")).deferredByDrain).toBe(
      "deferred by the coord device drain",
    );
  });

  it("reports no deferral for a normal (or empty) restore set", async () => {
    mockInvoke.mockResolvedValueOnce({
      success: true,
      data: { sessions: [rec({ claudeSessionId: "A" })] },
    });
    const normal = await fetchRestoreSet("default");
    expect(normal.deferredByDrain).toBeNull();
    expect(normal.records.map((r) => r.claudeSessionId)).toEqual(["A"]);

    mockInvoke.mockResolvedValueOnce({ success: true, data: { sessions: [] } });
    const empty = await fetchRestoreSet("default");
    expect(empty).toEqual({ records: [], deferredByDrain: null });
  });

  it("keeps fetchOpenRecords' contract: a deferral yields no records", async () => {
    mockInvoke.mockResolvedValueOnce({
      success: true,
      data: { sessions: [], deferredByDrain: { reason: "x" } },
    });
    expect(await fetchOpenRecords("default")).toEqual([]);
  });
});

describe("mayClaimRecordedZone — drain re-run merges with existing tabs", () => {
  it("lets a first restore claim any recorded zone", () => {
    expect(mayClaimRecordedZone(false, 2, "t-new", { 2: "t-operator" })).toBe(true);
  });

  it("never lets a drain re-run take a zone an operator tab holds", () => {
    expect(mayClaimRecordedZone(true, 2, "t-new", { 2: "t-operator" })).toBe(false);
    expect(mayClaimRecordedZone(true, 2, "t-new", {})).toBe(true);
    expect(mayClaimRecordedZone(true, 2, "t-same", { 2: "t-same" })).toBe(true);
  });

  it("never claims an unzoned record", () => {
    expect(mayClaimRecordedZone(false, -1, "t", {})).toBe(false);
  });
});

describe("reportTreeReset — drain deferral", () => {
  beforeEach(() => {
    mockInvoke.mockReset();
  });

  it("reports the open-record count as unknown while the drain withholds the set", async () => {
    mockInvoke.mockImplementation((cmd: unknown) =>
      cmd === "terminal_session_list_open"
        ? Promise.resolve({
            success: true,
            data: { sessions: [], deferredByDrain: { state: "drained", reason: "x" } },
          })
        : Promise.resolve({ success: true, message: null, data: null }),
    );
    await reportTreeReset({ mountNumber: 2, pageIds: ["default"] });
    expect(mockInvoke).toHaveBeenCalledWith(
      "terminal_report_tree_reset",
      expect.objectContaining({ mountNumber: 2, openRecordCount: undefined }),
    );
  });
});

describe("waitForHolderSweep — PTY-holder boot sweep before the restore", () => {
  beforeEach(() => {
    mockInvoke.mockReset();
  });

  it("asks the runner to wait for the sweep to settle", async () => {
    mockInvoke.mockResolvedValueOnce({ success: true, message: null, data: { settled: true } });
    await expect(waitForHolderSweep()).resolves.toBe(true);
    expect(mockInvoke).toHaveBeenCalledWith("terminal_holder_sweep_settled");
  });

  it("reports a sweep that did not settle in time", async () => {
    mockInvoke.mockResolvedValueOnce({ success: true, message: null, data: { settled: false } });
    await expect(waitForHolderSweep()).resolves.toBe(false);
  });

  it("counts a runner with no such command as settled (no sweep there)", async () => {
    mockInvoke.mockRejectedValueOnce(new Error("command terminal_holder_sweep_settled not found"));
    await expect(waitForHolderSweep()).resolves.toBe(true);
  });
});

describe("recheckRefusedAfterSweep — sessions refused as live before the sweep settled", () => {
  beforeEach(() => {
    mockInvoke.mockReset();
  });

  const unsettled = { success: true, message: null, data: { settled: false } };
  const settled = { success: true, message: null, data: { settled: true } };

  it("once the sweep settles, returns exactly the refused ids no longer hosted live", async () => {
    mockInvoke.mockResolvedValueOnce(unsettled).mockResolvedValueOnce(settled);
    const live = vi.fn().mockResolvedValue(new Set(["still-alive", "never-refused"]));
    await expect(
      recheckRefusedAfterSweep(["reaped-by-sweep", "still-alive"], live),
    ).resolves.toEqual(["reaped-by-sweep"]);
    expect(mockInvoke).toHaveBeenCalledTimes(2);
    expect(live).toHaveBeenCalledTimes(1);
  });

  it("resumes nothing when the sweep never settles", async () => {
    mockInvoke.mockResolvedValue(unsettled);
    const live = vi.fn().mockResolvedValue(new Set<string>());
    await expect(recheckRefusedAfterSweep(["a"], live)).resolves.toEqual([]);
    expect(mockInvoke).toHaveBeenCalledTimes(HOLDER_SWEEP_RECHECK_ATTEMPTS);
    expect(live).not.toHaveBeenCalled();
  });

  it("fails closed when the live registry cannot be read", async () => {
    mockInvoke.mockResolvedValue(settled);
    const live = vi.fn().mockResolvedValue(null);
    await expect(recheckRefusedAfterSweep(["a"], live)).resolves.toEqual([]);
  });

  it("does nothing for nothing refused", async () => {
    await expect(recheckRefusedAfterSweep([])).resolves.toEqual([]);
    expect(mockInvoke).not.toHaveBeenCalled();
  });
});

describe("the holder sweep's census — restore what is not held, now", () => {
  beforeEach(() => {
    mockInvoke.mockReset();
  });

  const rec = (terminalId: string) => ({ terminalId, claudeSessionId: `s-${terminalId}` });

  it("restores a non-held record without waiting while a held one is still adopting", async () => {
    // The census is in: t2's pane is still being adopted, the sweep is unsettled.
    mockInvoke.mockImplementation(
      (cmd: unknown) =>
        cmd === "terminal_holder_sweep_status"
          ? Promise.resolve({
              success: true,
              message: null,
              data: { settled: false, held: ["t2"] },
            })
          : new Promise(() => {}), // a settle-wait would never resolve
    );
    const status = await fetchHolderSweepStatus();
    expect(mockInvoke).toHaveBeenCalledTimes(1);
    expect(mockInvoke).toHaveBeenCalledWith("terminal_holder_sweep_status");
    expect(mockInvoke).not.toHaveBeenCalledWith("terminal_holder_sweep_settled");
    const { now, heldBack } = splitRestoreByHold([rec("t1"), rec("t2")], status);
    expect(now.map((r) => r.terminalId)).toEqual(["t1"]);
    expect(heldBack.map((r) => r.terminalId)).toEqual(["t2"]);
  });

  it("holds nothing back once the sweep settled, or with no census", () => {
    const records = [rec("t1"), rec("t2")];
    expect(splitRestoreByHold(records, { settled: true, held: new Set(["t2"]) }).heldBack).toEqual(
      [],
    );
    expect(splitRestoreByHold(records, { settled: false, held: null }).now).toHaveLength(2);
  });

  it("reads a runner without the command as settled with nothing held", async () => {
    mockInvoke.mockRejectedValueOnce(new Error("unknown command"));
    const status = await fetchHolderSweepStatus();
    expect(status.settled).toBe(true);
    expect(status.held?.size).toBe(0);
  });

  it("reads a census that did not come in as held: null", async () => {
    mockInvoke.mockResolvedValueOnce({
      success: true,
      message: null,
      data: { settled: false, held: null },
    });
    await expect(fetchHolderSweepStatus()).resolves.toEqual({ settled: false, held: null });
  });

  it("re-runs the held-back sessions after the sweep, settled or not", async () => {
    mockInvoke.mockResolvedValue({ success: true, message: null, data: { settled: false } });
    const live = vi.fn().mockResolvedValue(new Set<string>());
    await expect(recheckRefusedAfterSweep([], live, ["s-t2"])).resolves.toEqual(["s-t2"]);
    expect(live).not.toHaveBeenCalled();
  });
});

describe("onHolderSessionReleased — a deferred holder died before adoption", () => {
  beforeEach(() => {
    mockInvoke.mockReset();
    mockInvoke.mockResolvedValue({ success: true, message: null, data: { settled: true } });
  });

  const payload = { sessionId: "sess-1", terminalId: "t-pane", pageId: "page-2" };

  it("re-runs the restore of its page for that session when no live process hosts it", async () => {
    const schedule = vi.fn();
    const live = vi.fn().mockResolvedValue(new Set<string>());
    await expect(onHolderSessionReleased(payload, schedule, live)).resolves.toBe(true);
    expect(schedule).toHaveBeenCalledWith("page-2", ["sess-1"]);
  });

  it("leaves a session a live process still hosts alone", async () => {
    const schedule = vi.fn();
    const live = vi.fn().mockResolvedValue(new Set(["sess-1"]));
    await expect(onHolderSessionReleased(payload, schedule, live)).resolves.toBe(false);
    expect(schedule).not.toHaveBeenCalled();
  });

  it("re-runs the default page when the record named none", async () => {
    const schedule = vi.fn();
    const live = vi.fn().mockResolvedValue(new Set<string>());
    await onHolderSessionReleased({ ...payload, pageId: null }, schedule, live);
    expect(schedule).toHaveBeenCalledWith("default", ["sess-1"]);
  });
});

describe("restore re-runs — one run per page, one cold resume per session (review round 7, U1)", () => {
  beforeEach(() => {
    mockInvoke.mockReset();
    mockInvoke.mockResolvedValue({ success: true, message: null, data: { settled: true } });
  });

  /**
   * The hook's wiring, minus React: `schedule` is `scheduleRecheckRerun`,
   * `run` is the init effect — `begin` at its start, a guarded cold resume per
   * record, `finish` (and a trigger) in its `finally`.
   */
  function harness() {
    const queue = new RestoreRerunQueue();
    const guard = new ColdResumeGuard();
    const createTerminal = vi.fn();
    const triggered: string[] = [];
    const schedule = (pageId: string, ids: string[]) => {
      if (queue.schedule(pageId, ids)) triggered.push(pageId);
    };
    const runBody = (pageId: string, records: readonly string[]) => {
      const only = queue.begin(pageId);
      const claims: string[] = [];
      for (const id of only ? [...only] : records) {
        if (!guard.tryClaim(id)) continue;
        claims.push(id);
        createTerminal(id);
      }
      return () => {
        for (const id of claims) guard.release(id);
        if (queue.finish(pageId, "restored") === 0) triggered.push(pageId);
      };
    };
    return { queue, guard, createTerminal, triggered, schedule, runBody };
  }

  it("two schedules for one session while a run is in flight produce exactly one cold resume", async () => {
    const h = harness();
    const live = vi.fn().mockResolvedValue(new Set<string>());
    // The page's first restore is in flight; it refused sess-1 as live.
    const endFirst = h.runBody("page-2", []);
    // A late sweep settle and a released holder both ask for sess-1.
    const released = { sessionId: "sess-1", terminalId: "t-pane", pageId: "page-2" };
    await onHolderSessionReleased(released, h.schedule, live);
    await onHolderSessionReleased(released, h.schedule, live);
    expect(h.triggered).toEqual([]);
    endFirst();
    // ONE re-run, for the merged ids.
    expect(h.triggered).toEqual(["page-2"]);
    h.runBody("page-2", ["sess-1", "sess-other"])();
    expect(h.createTerminal).toHaveBeenCalledTimes(1);
    expect(h.createTerminal).toHaveBeenCalledWith("sess-1");
    expect(h.triggered).toEqual(["page-2"]);
  });

  it("a schedule with no run in flight triggers at once", () => {
    const h = harness();
    h.runBody("page-2", [])();
    h.schedule("page-2", ["sess-1"]);
    expect(h.triggered).toEqual(["page-2"]);
    expect(h.queue.begin("page-2")).toEqual(new Set(["sess-1"]));
  });

  it("a page this window never began restoring is not re-run: its full restore covers it", () => {
    const h = harness();
    h.schedule("page-9", ["sess-1"]);
    expect(h.triggered).toEqual([]);
    expect(h.queue.begin("page-9")).toBeUndefined();
  });

  it("a drain-deferred run drops what was scheduled meanwhile: the drain lift re-runs in full", () => {
    const queue = new RestoreRerunQueue();
    queue.begin("page-2");
    expect(queue.schedule("page-2", ["sess-1"])).toBe(false);
    expect(queue.finish("page-2", "deferred")).toBeNull();
    expect(queue.begin("page-2")).toBeUndefined();
    expect(queue.finish("page-2", "restored")).toBeNull();
  });

  it("the session guard keeps two concurrent runs from both creating a tab", () => {
    const guard = new ColdResumeGuard();
    expect(guard.tryClaim("sess-1")).toBe(true);
    expect(guard.tryClaim("sess-1")).toBe(false);
    guard.release("sess-1");
    expect(guard.tryClaim("sess-1")).toBe(true);
  });
});

describe("RestoreRerunQueue — an aborted restore keeps what it owes (review V1)", () => {
  it("(a) an abort with pending ids still re-runs them, after a backoff", () => {
    const queue = new RestoreRerunQueue();
    // The page's first restore is in flight; a released session arrives.
    queue.begin("page-2");
    expect(queue.schedule("page-2", ["sess-1"])).toBe(false);
    // The run aborts (indeterminate reconnect): the id is kept, and the
    // queue asks for a re-run after the first backoff.
    expect(queue.finish("page-2", "aborted")).toBe(ABORT_RETRY_BASE_MS);
    // That re-run is the page's FULL restore (none ever completed), which
    // covers sess-1.
    expect(queue.begin("page-2")).toBeUndefined();
    expect(queue.finish("page-2", "restored")).toBeNull();
  });

  it("(a) an aborted targeted re-run gives its ids back, and the retry restores exactly them", () => {
    const queue = new RestoreRerunQueue();
    queue.begin("page-2");
    queue.finish("page-2", "restored");
    expect(queue.schedule("page-2", ["sess-1"])).toBe(true);
    expect(queue.begin("page-2")).toEqual(new Set(["sess-1"]));
    expect(queue.finish("page-2", "aborted")).toBe(ABORT_RETRY_BASE_MS);
    expect(queue.begin("page-2")).toEqual(new Set(["sess-1"]));
    expect(queue.finish("page-2", "restored")).toBeNull();
    expect(queue.begin("page-2")).toBeUndefined();
  });

  it("(b) a schedule after an abort is kept, and triggers a re-run", () => {
    const queue = new RestoreRerunQueue();
    queue.begin("page-2");
    // Aborted with nothing owed: no re-run of its own.
    expect(queue.finish("page-2", "aborted")).toBeNull();
    // The page stays known: a later released session is kept, not ignored.
    expect(queue.schedule("page-2", ["sess-1"])).toBe(true);
    expect(queue.restoredOnce("page-2")).toBe(false);
    expect(queue.begin("page-2")).toBeUndefined();
    expect(queue.finish("page-2", "restored")).toBeNull();
    expect(queue.restoredOnce("page-2")).toBe(true);
  });

  it("(c) consecutive aborts retry at most ABORT_RETRY_LIMIT times, with a doubling backoff", () => {
    const queue = new RestoreRerunQueue();
    queue.begin("page-2");
    queue.schedule("page-2", ["sess-1"]);
    const delays: Array<number | null> = [queue.finish("page-2", "aborted")];
    // Drive every retry the queue asks for; each one aborts too.
    while (delays[delays.length - 1] !== null && delays.length < 50) {
      queue.begin("page-2");
      delays.push(queue.finish("page-2", "aborted"));
    }
    expect(delays).toEqual([
      ...Array.from({ length: ABORT_RETRY_LIMIT }, (_, i) => ABORT_RETRY_BASE_MS * 2 ** i),
      null,
    ]);
    // The id is still owed: the next schedule (or activation) re-runs the
    // page, and a completed restore resets the streak.
    expect(queue.schedule("page-2", ["sess-2"])).toBe(true);
    queue.begin("page-2");
    expect(queue.finish("page-2", "restored")).toBeNull();
    queue.schedule("page-2", ["sess-3"]);
    queue.begin("page-2");
    expect(queue.finish("page-2", "aborted")).toBe(ABORT_RETRY_BASE_MS);
  });
});

describe("restore re-run wiring — the hook's timer, trigger and unmount (review V1, r1)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  /**
   * The hook's wiring with its effect replaced by a recorder: `trigger` is
   * `triggerRerun`, and a "run" is `beginRestoreRun` … `settleRestoreRun`,
   * exactly as the init effect calls them. `drainDeferred` is the hook's
   * `drainDeferredPages`, and a page in it re-runs merging.
   */
  function wiring() {
    const drainDeferred = new Set<string>();
    const runs: Array<{ pageId: string; merge: boolean }> = [];
    const w: RestoreRerunWiring = {
      queue: new RestoreRerunQueue(),
      timers: new RerunTimers(),
      markMerging: (pageId) => drainDeferred.add(pageId),
      trigger: (pageId, merge) => {
        if (merge) drainDeferred.add(pageId);
        runs.push({ pageId, merge: merge || drainDeferred.has(pageId) });
      },
    };
    // The init effect's own start: it consumes the page's merge mark.
    const begin = (pageId: string) => {
      const merging = drainDeferred.delete(pageId);
      return { only: beginRestoreRun(w, pageId), merging };
    };
    return { w, runs, drainDeferred, begin };
  }

  it("a backoff retry that a schedule beat to it starts no second restore (finding 1)", () => {
    const { w, runs, begin } = wiring();
    // The page's first restore is in flight and owes sess-1; it aborts.
    begin("page-2");
    scheduleRestoreRerun(w, "page-2", ["sess-1"]);
    settleRestoreRun(w, "page-2", "aborted", false);
    expect(runs).toEqual([]);
    expect(vi.getTimerCount()).toBe(1);
    // Before the backoff fires, another released session wins the race: it
    // re-runs the page at once, and that run restores.
    scheduleRestoreRerun(w, "page-2", ["sess-2"]);
    expect(runs).toEqual([{ pageId: "page-2", merge: false }]);
    const { only } = begin("page-2");
    expect(only).toBeUndefined();
    // The run beginning cancelled the page's backoff retry.
    expect(vi.getTimerCount()).toBe(0);
    settleRestoreRun(w, "page-2", "restored", false);
    // The pending retry is moot: no further run, however long we wait.
    vi.advanceTimersByTime(60_000);
    expect(runs).toHaveLength(1);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("a retry that fires owing nothing does nothing, and decides merging when it fires (finding 1)", () => {
    const { w, runs, begin } = wiring();
    begin("page-2");
    settleRestoreRun(w, "page-2", "restored", false);
    // A targeted re-run aborts while owing sess-1: retried after the backoff,
    // merging — the page has a completed restore.
    scheduleRestoreRerun(w, "page-2", ["sess-1"]);
    runs.length = 0;
    begin("page-2");
    settleRestoreRun(w, "page-2", "aborted", false);
    vi.advanceTimersByTime(ABORT_RETRY_BASE_MS);
    expect(runs).toEqual([{ pageId: "page-2", merge: true }]);
    // Defence in depth: a retry still armed when its ids were taken by a run
    // that did not cancel it (the queue driven directly) fires into nothing.
    runs.length = 0;
    scheduleRestoreRerun(w, "page-2", ["sess-3"]);
    runs.length = 0;
    begin("page-2");
    settleRestoreRun(w, "page-2", "aborted", false);
    expect(vi.getTimerCount()).toBe(1);
    w.queue.begin("page-2");
    w.queue.finish("page-2", "restored");
    vi.advanceTimersByTime(60_000);
    expect(runs).toEqual([]);
  });

  it("a run that ends after unmount schedules nothing (finding 2)", () => {
    const { w, runs, begin } = wiring();
    begin("page-2");
    scheduleRestoreRerun(w, "page-2", ["sess-1"]);
    w.timers.setActive(false);
    settleRestoreRun(w, "page-2", "aborted", false);
    expect(vi.getTimerCount()).toBe(0);
    vi.advanceTimersByTime(60_000);
    expect(runs).toEqual([]);
  });

  it("unmount clears a pending retry; a re-mount re-arms scheduling", () => {
    const { w, runs, begin } = wiring();
    begin("page-2");
    scheduleRestoreRerun(w, "page-2", ["sess-1"]);
    settleRestoreRun(w, "page-2", "aborted", false);
    w.timers.setActive(false);
    expect(vi.getTimerCount()).toBe(0);
    w.timers.setActive(true);
    begin("page-2");
    settleRestoreRun(w, "page-2", "aborted", false);
    vi.advanceTimersByTime(ABORT_RETRY_BASE_MS * 2);
    expect(runs).toEqual([{ pageId: "page-2", merge: false }]);
  });

  it("an aborted merging re-run re-runs merging on every path (finding 3)", () => {
    // Via the backoff timer.
    {
      const { w, runs, drainDeferred, begin } = wiring();
      drainDeferred.add("page-2"); // the drain lifted: a merging re-run
      const { merging } = begin("page-2");
      expect(merging).toBe(true);
      scheduleRestoreRerun(w, "page-2", ["sess-1"]);
      settleRestoreRun(w, "page-2", "aborted", merging);
      expect(drainDeferred.has("page-2")).toBe(true);
      vi.advanceTimersByTime(ABORT_RETRY_BASE_MS);
      expect(runs).toEqual([{ pageId: "page-2", merge: true }]);
    }
    // Via a later schedule (the page never completed a restore).
    {
      const { w, runs, drainDeferred, begin } = wiring();
      drainDeferred.add("page-2");
      const { merging } = begin("page-2");
      settleRestoreRun(w, "page-2", "aborted", merging);
      scheduleRestoreRerun(w, "page-2", ["sess-1"]);
      expect(runs).toEqual([{ pageId: "page-2", merge: true }]);
    }
    // Via re-activation: the effect's own start finds the merge mark.
    {
      const { w, drainDeferred, begin } = wiring();
      drainDeferred.add("page-2");
      const { merging } = begin("page-2");
      settleRestoreRun(w, "page-2", "aborted", merging);
      expect(begin("page-2").merging).toBe(true);
    }
    // An aborted FIRST restore stays a first restore.
    {
      const { w, drainDeferred, begin } = wiring();
      const { merging } = begin("page-2");
      settleRestoreRun(w, "page-2", "aborted", merging);
      expect(drainDeferred.has("page-2")).toBe(false);
    }
  });
});

describe("ownsReleasedPage — one window handles a released session (review round 7, N4)", () => {
  const bindings = { "page-pop": "term-1" };

  it("the main window owns every page not detached into a pop-out", () => {
    expect(ownsReleasedPage("page-2", null, bindings)).toBe(true);
    expect(ownsReleasedPage("default", null, bindings)).toBe(true);
    expect(ownsReleasedPage("page-pop", null, bindings)).toBe(false);
  });

  it("a pinned pop-out owns its one page only", () => {
    expect(ownsReleasedPage("page-pop", "page-pop", bindings)).toBe(true);
    expect(ownsReleasedPage("page-2", "page-pop", bindings)).toBe(false);
  });

  it("exactly one window owns each page", () => {
    for (const page of ["page-2", "page-pop", "default"]) {
      const owners = [null, "page-pop"].filter((pinned) =>
        ownsReleasedPage(page, pinned, bindings),
      );
      expect(owners).toHaveLength(1);
    }
  });
});

describe("reconnectedTabFor (restore branch (a))", () => {
  const live = new Set(["tab-1"]);

  it("reconnects an ordinary record to its live tab", () => {
    expect(reconnectedTabFor(rec({ terminalId: "tab-1" }), live)).toBe("tab-1");
  });

  it("never reconnects a held record into a live tab, even on its old terminal id", () => {
    // The held record's old terminal id now hosts another session's live tab;
    // reconnecting would stamp that tab with the held session's id.
    const held = rec({ terminalId: "tab-1", awaitingAccountSince: 1_000 });
    expect(reconnectedTabFor(held, live)).toBeNull();
  });

  it("never reconnects an unbound record", () => {
    expect(reconnectedTabFor(rec({ terminalId: "" }), new Set([""]))).toBeNull();
  });

  it("does not reconnect a record whose terminal is not live", () => {
    expect(reconnectedTabFor(rec({ terminalId: "tab-9" }), live)).toBeNull();
  });
});

describe("holdNeedsAccountIfNotLive (never hold or detach a live session)", () => {
  const entry = {
    record: rec({ claudeSessionId: "held-1" }),
    reason: "no-transcript-holder" as const,
  };
  const run = async (live: ReadonlySet<string> | null) => {
    const note = vi.fn();
    const decision = await holdNeedsAccountIfNotLive({
      entry,
      getLiveSessionIds: async () => live,
      noteNeedsAccount: note,
    });
    return { decision, note };
  };

  it("a session a live process hosts is NOT held (no mark, no detach)", async () => {
    const { decision, note } = await run(new Set(["held-1"]));
    expect(decision).toBe("skip-alive");
    expect(note).not.toHaveBeenCalled();
  });

  it("an unreadable live registry surfaces the record for a choice but does NOT re-mark it", async () => {
    const { decision, note } = await run(null);
    expect(decision).toBe("skip-unknown");
    expect(note).toHaveBeenCalledTimes(1);
    expect(note).toHaveBeenCalledWith(entry, { mark: false });
  });

  it("a session no live process hosts IS held", async () => {
    const { decision, note } = await run(new Set(["other"]));
    expect(decision).toBe("respawn");
    expect(note).toHaveBeenCalledTimes(1);
    expect(note).toHaveBeenCalledWith(entry, { mark: true });
  });
});

describe("applyNeedsAccountNote (mark: false never touches the registry row)", () => {
  const entry = {
    record: rec({ claudeSessionId: "held-2" }),
    reason: "no-transcript-holder" as const,
  };
  const run = (mark: boolean, prior: NeedsAccountRecord[] = []) => {
    let records = prior;
    const invoke = vi.fn().mockResolvedValue(undefined);
    applyNeedsAccountNote(
      entry,
      { mark },
      {
        setRecords: (update) => {
          records = update(records);
        },
        invoke,
      },
    );
    return { records, invoke };
  };

  it("mark: false lists the record but never invokes the mark (no stamp, no detach)", () => {
    const { records, invoke } = run(false);
    expect(records).toEqual([entry]);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("mark: true lists the record AND holds the row", () => {
    const { records, invoke } = run(true);
    expect(records).toEqual([entry]);
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith("terminal_session_mark_awaiting_account", {
      claudeSessionId: "held-2",
    });
  });

  it("replaces an earlier entry for the same session and keeps the others", () => {
    const older = {
      record: rec({ claudeSessionId: "held-2" }),
      reason: "ambiguous-transcript" as const,
    };
    const other = {
      record: rec({ claudeSessionId: "other" }),
      reason: "no-transcript-holder" as const,
    };
    const { records } = run(false, [older, other]);
    expect(records).toEqual([other, entry]);
  });

  it("a rejected mark is logged, never thrown", async () => {
    const invoke = vi.fn().mockRejectedValue(new Error("boom"));
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      expect(() =>
        applyNeedsAccountNote(entry, { mark: true }, { setRecords: () => {}, invoke }),
      ).not.toThrow();
      await Promise.resolve();
      await Promise.resolve();
      expect(warn).toHaveBeenCalledWith(
        expect.stringContaining("held-2"),
        expect.objectContaining({ message: "boom" }),
      );
    } finally {
      warn.mockRestore();
    }
  });
});
